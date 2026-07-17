//! WebSocket Handler — 连接管理、消息解析和路由分发
//!
//! 负责:
//!   1. WebSocket 升级握手
//!   2. 连接建立后推送初始状态（会话列表、活跃会话、人格列表）
//!   3. 进入消息循环，路由客户端消息到 Orchestrator / SessionManager / PersonaManager
//!   4. 将 ProgressEvent 转发为 WebProgressEvent 给前端

use std::collections::{HashMap, HashSet};
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
    pub active_query_sessions: Arc<Mutex<HashSet<String>>>,
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
    /// 应用层心跳（浏览器无法发送原生 Ping 帧）
    Heartbeat,
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
    New { input: String },
    Edit { message_id: String, content: String },
    Retry { message_id: String },
}

struct PreparedWebQuery {
    turn: UserQueryTurn,
    memory_scope: ConversationMemoryScope,
    restore_history: Option<Vec<ChatMessageRestore>>,
    updated_messages: Option<Vec<ChatMessage>>,
}

async fn claim_query_session(state: &Arc<AppState>, session_id: &str) -> bool {
    state
        .active_query_sessions
        .lock()
        .await
        .insert(session_id.to_string())
}

async fn release_query_session(state: &Arc<AppState>, session_id: &str) {
    state.active_query_sessions.lock().await.remove(session_id);
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
    if !claim_query_session(state, &session_id).await {
        return Err("当前会话已有查询正在进行，请等待完成".into());
    }

    let prepared = match action {
        QueryAction::New { input } => {
            let mut sessions = state.sessions.lock().await;
            if sessions.active().id != session_id {
                Err("活跃会话已变化，请重新发送".to_string())
            } else {
                sessions.push_user_query(&input).map(|turn| {
                    let messages = sessions.active_visible_messages();
                    let restore_history = restore_messages_before_turn(&messages, &turn.message_id);
                    (turn, Some(restore_history), Some(messages))
                })
            }
        }
        QueryAction::Edit {
            message_id,
            content,
        } => {
            prepare_conversation_fork(state, &session_id, |sessions| {
                sessions.edit_user_message(&message_id, &content)
            })
            .await
        }
        QueryAction::Retry { message_id } => {
            prepare_conversation_fork(state, &session_id, |sessions| {
                sessions.retry_last_user_message(&message_id)
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
                Err(error) => {
                    release_query_session(state, &session_id).await;
                    return Err(format!("创建对话记忆代次失败: {error}"));
                }
            };
            Ok(PreparedWebQuery {
                turn,
                memory_scope,
                restore_history,
                updated_messages,
            })
        }
        Err(error) => {
            release_query_session(state, &session_id).await;
            Err(error)
        }
    }
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

            // ── 分支1: 客户端消息（始终活跃） ──
            maybe_msg = receiver.next() => {
                match maybe_msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(client_msg) => {
                                let query_action = match client_msg {
                                    ClientMessage::Query { input } => Some(QueryAction::New { input }),
                                    ClientMessage::EditUserMessage { message_id, content } => {
                                        Some(QueryAction::Edit { message_id, content })
                                    }
                                    ClientMessage::RetryLastUserMessage { message_id } => {
                                        Some(QueryAction::Retry { message_id })
                                    }
                                    ClientMessage::Cancel => {
                                    if let Some(ref ct) = cancel_token {
                                        ct.cancel();
                                        info!("已取消当前查询 (CancellationToken)");
                                    }
                                    if let Some(handle) = query_handle.take() {
                                        handle.abort();
                                    }
                                    // 将已收集的 assistant 回复记录到会话
                                    if !assistant_text.is_empty() {
                                        let mut sessions = state.sessions.lock().await;
                                        if let Some(ref sid) = query_session_id {
                                            sessions.push_message_to(sid, "assistant", &assistant_text);
                                        }
                                    }
                                    if let Some(ref sid) = query_session_id {
                                        release_query_session(&state, sid).await;
                                    }
                                    query_rx = None;
                                    query_session_id = None;
                                    query_generation_id = None;
                                    cancel_token = None;
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
                                if query_rx.is_some() || query_handle.is_some() {
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
                                                release_query_session(&state, &prepared.turn.session_id).await;
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
                if let Some(ref sid) = session_id {
                    release_query_session(&state, sid).await;
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
    if let Some(session_id) = query_session_id.take() {
        release_query_session(&state, &session_id).await;
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
        | ClientMessage::Cancel => {
            // 已在 handle_socket 的 select! 中直接处理
        }
        ClientMessage::Heartbeat => {
            // 应用层心跳 — 浏览器无法发送原生 Ping，用 JSON 消息替代
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
}
