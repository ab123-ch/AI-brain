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
    InboxState, LegacyMessageSeed, MemberAddress, PostMessageResult, RoomEventPage, RoomInputMode,
    RoomSnapshot, DEFAULT_THREAD_KEY,
};
use crate::web::collaboration_runtime::{CollaborationRuntime, RoomWorkingDirectoryUpdateError};
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
        #[serde(default)]
        reply_to_event_id: Option<String>,
    },
    UpdateRoomWorkingDirectory {
        working_directory: String,
        expected_room_version: u64,
    },
    LoadRoomEventsBefore {
        before_sequence: u64,
        #[serde(default = "default_room_event_page_limit")]
        limit: usize,
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

const fn default_room_event_page_limit() -> usize {
    100
}

#[async_trait::async_trait]
trait RoomProtocolRuntime: Send + Sync {
    async fn update_room_working_directory(
        &self,
        room_id: String,
        working_directory: String,
        expected_room_version: u64,
    ) -> Result<RoomSnapshot, RoomWorkingDirectoryUpdateError>;

    async fn snapshot(&self, room_id: String) -> Result<RoomSnapshot, String>;

    async fn events_before(
        &self,
        room_id: String,
        before_sequence: u64,
        limit: usize,
    ) -> Result<RoomEventPage, String>;

    #[allow(clippy::too_many_arguments)]
    async fn post_message_checked(
        &self,
        room_id: String,
        recipients: Vec<MemberAddress>,
        content: String,
        mode: RoomInputMode,
        thread_key: String,
        expected_room_version: u64,
        command_id: String,
        reply_to_event_id: Option<String>,
    ) -> Result<PostMessageResult, String>;
}

#[async_trait::async_trait]
impl RoomProtocolRuntime for CollaborationRuntime {
    async fn update_room_working_directory(
        &self,
        room_id: String,
        working_directory: String,
        expected_room_version: u64,
    ) -> Result<RoomSnapshot, RoomWorkingDirectoryUpdateError> {
        CollaborationRuntime::update_room_working_directory_classified(
            self,
            room_id,
            working_directory,
            expected_room_version,
        )
        .await
    }

    async fn snapshot(&self, room_id: String) -> Result<RoomSnapshot, String> {
        CollaborationRuntime::snapshot(self, room_id).await
    }

    async fn events_before(
        &self,
        room_id: String,
        before_sequence: u64,
        limit: usize,
    ) -> Result<RoomEventPage, String> {
        CollaborationRuntime::events_before(self, room_id, before_sequence, limit).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn post_message_checked(
        &self,
        room_id: String,
        recipients: Vec<MemberAddress>,
        content: String,
        mode: RoomInputMode,
        thread_key: String,
        expected_room_version: u64,
        command_id: String,
        reply_to_event_id: Option<String>,
    ) -> Result<PostMessageResult, String> {
        CollaborationRuntime::post_message_checked(
            self,
            room_id,
            recipients,
            content,
            mode,
            thread_key,
            expected_room_version,
            command_id,
            reply_to_event_id,
        )
        .await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RoomOperationErrorIdentity {
    Post {
        room_id: String,
        command_id: String,
    },
    Directory {
        room_id: String,
        expected_room_version: u64,
        working_directory: String,
    },
    Pagination {
        room_id: String,
        before_sequence: u64,
    },
}

enum RoomProtocolDispatch {
    Handled {
        events: Vec<WebProgressEvent>,
        error_identity: Option<RoomOperationErrorIdentity>,
    },
    Unhandled(ClientMessage),
}

async fn dispatch_room_protocol_message(
    runtime: &dyn RoomProtocolRuntime,
    active_room_id: String,
    message: ClientMessage,
) -> RoomProtocolDispatch {
    let (events, error_identity) = match message {
        ClientMessage::UpdateRoomWorkingDirectory {
            working_directory,
            expected_room_version,
        } => {
            let identity = RoomOperationErrorIdentity::Directory {
                room_id: active_room_id.clone(),
                expected_room_version,
                working_directory: working_directory.clone(),
            };
            match runtime
                .update_room_working_directory(
                    active_room_id.clone(),
                    working_directory,
                    expected_room_version,
                )
                .await
            {
                Ok(_) => (Vec::new(), None),
                Err(error) => {
                    let version_conflict = error.is_version_conflict();
                    let mut events = vec![WebProgressEvent::Error {
                        message: error.into_message(),
                    }];
                    if version_conflict {
                        match runtime.snapshot(active_room_id).await {
                            Ok(snapshot) => {
                                events.push(WebProgressEvent::RoomSnapshot { snapshot })
                            }
                            Err(error) => events.push(WebProgressEvent::Error {
                                message: format!("刷新协作房间快照失败: {error}"),
                            }),
                        }
                    }
                    (events, Some(identity))
                }
            }
        }
        ClientMessage::LoadRoomEventsBefore {
            before_sequence,
            limit,
        } => {
            let identity = RoomOperationErrorIdentity::Pagination {
                room_id: active_room_id.clone(),
                before_sequence,
            };
            match runtime
                .events_before(active_room_id.clone(), before_sequence, limit)
                .await
            {
                Ok(page) => (
                    vec![WebProgressEvent::RoomEventsLoadedBefore {
                        room_id: active_room_id,
                        before_sequence,
                        has_more: page.has_more,
                        events: page.events,
                    }],
                    None,
                ),
                Err(error) => (
                    vec![WebProgressEvent::Error { message: error }],
                    Some(identity),
                ),
            }
        }
        ClientMessage::PostRoomMessage {
            recipients,
            content,
            mode,
            thread_key,
            expected_room_version,
            command_id,
            reply_to_event_id,
        } => {
            let command_id = if command_id.trim().is_empty() {
                format!("web-{}", uuid::Uuid::new_v4())
            } else {
                command_id
            };
            match runtime
                .post_message_checked(
                    active_room_id.clone(),
                    recipients,
                    content,
                    mode,
                    thread_key,
                    expected_room_version,
                    command_id.clone(),
                    reply_to_event_id,
                )
                .await
            {
                Ok(result) => (
                    vec![WebProgressEvent::RoomMessageAccepted {
                        room_id: active_room_id,
                        command_id,
                        event_id: result.event.event_id,
                        duplicate: result.duplicate,
                    }],
                    None,
                ),
                Err(error) => (
                    vec![WebProgressEvent::Error { message: error }],
                    Some(RoomOperationErrorIdentity::Post {
                        room_id: active_room_id,
                        command_id,
                    }),
                ),
            }
        }
        message => return RoomProtocolDispatch::Unhandled(message),
    };
    RoomProtocolDispatch::Handled {
        events,
        error_identity,
    }
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
            None,
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
        | WebProgressEvent::RoomEventsLoadedBefore { room_id, .. }
        | WebProgressEvent::RoomMessageAccepted { room_id, .. }
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
                                                    pending_legacy_query = None;
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
    let msg = match dispatch_room_protocol_message(
        state.collaboration.as_ref(),
        active_room_id(state).await,
        msg,
    )
    .await
    {
        RoomProtocolDispatch::Handled {
            events,
            mut error_identity,
        } => {
            for event in events {
                let correlated_identity =
                    take_room_operation_error_identity(&event, &mut error_identity);
                let sent = match (event, correlated_identity.as_ref()) {
                    (WebProgressEvent::Error { message }, Some(identity)) => {
                        send_room_operation_error(sender, &message, identity).await
                    }
                    (event, _) => send_event(sender, event).await,
                };
                if sent.is_err() {
                    return;
                }
            }
            return;
        }
        RoomProtocolDispatch::Unhandled(message) => message,
    };

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
        ClientMessage::PostRoomMessage { .. }
        | ClientMessage::UpdateRoomWorkingDirectory { .. }
        | ClientMessage::LoadRoomEventsBefore { .. } => {
            unreachable!("房间协议消息必须由定向 dispatcher 处理")
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

fn room_operation_error_json(
    message: &str,
    identity: &RoomOperationErrorIdentity,
) -> serde_json::Value {
    match identity {
        RoomOperationErrorIdentity::Post {
            room_id,
            command_id,
        } => serde_json::json!({
            "type": "error",
            "message": message,
            "room_operation": "post",
            "room_id": room_id,
            "command_id": command_id,
        }),
        RoomOperationErrorIdentity::Directory {
            room_id,
            expected_room_version,
            working_directory,
        } => serde_json::json!({
            "type": "error",
            "message": message,
            "room_operation": "directory",
            "room_id": room_id,
            "expected_room_version": expected_room_version,
            "working_directory": working_directory,
        }),
        RoomOperationErrorIdentity::Pagination {
            room_id,
            before_sequence,
        } => serde_json::json!({
            "type": "error",
            "message": message,
            "room_operation": "pagination",
            "room_id": room_id,
            "before_sequence": before_sequence,
        }),
    }
}

fn take_room_operation_error_identity(
    event: &WebProgressEvent,
    identity: &mut Option<RoomOperationErrorIdentity>,
) -> Option<RoomOperationErrorIdentity> {
    if matches!(event, WebProgressEvent::Error { .. }) {
        identity.take()
    } else {
        None
    }
}

async fn send_room_operation_error(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &str,
    identity: &RoomOperationErrorIdentity,
) -> Result<(), axum::Error> {
    sender
        .send(Message::Text(
            room_operation_error_json(message, identity)
                .to_string()
                .into(),
        ))
        .await
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
    use crate::web::collaboration::{CollaborationActor, CollaborationConfig, RoomEventPage};
    use tokio::sync::Mutex as TokioMutex;

    struct TestRoomProtocol {
        repository: Arc<crate::web::collaboration::CollaborationRepository>,
        calls: TokioMutex<Vec<(String, String)>>,
    }

    impl TestRoomProtocol {
        fn new(repository: Arc<crate::web::collaboration::CollaborationRepository>) -> Self {
            Self {
                repository,
                calls: TokioMutex::new(Vec::new()),
            }
        }

        async fn record(&self, operation: &str, room_id: &str) {
            self.calls
                .lock()
                .await
                .push((operation.into(), room_id.into()));
        }
    }

    #[async_trait::async_trait]
    impl RoomProtocolRuntime for TestRoomProtocol {
        async fn update_room_working_directory(
            &self,
            room_id: String,
            working_directory: String,
            expected_room_version: u64,
        ) -> Result<RoomSnapshot, RoomWorkingDirectoryUpdateError> {
            self.record("update_directory", &room_id).await;
            self.repository
                .update_room_working_directory(&room_id, &working_directory, expected_room_version)
                .map_err(RoomWorkingDirectoryUpdateError::from)?;
            self.repository
                .snapshot(&room_id)
                .map_err(RoomWorkingDirectoryUpdateError::from)
        }

        async fn snapshot(&self, room_id: String) -> Result<RoomSnapshot, String> {
            self.record("snapshot", &room_id).await;
            self.repository
                .snapshot(&room_id)
                .map_err(|error| error.to_string())
        }

        async fn events_before(
            &self,
            room_id: String,
            before_sequence: u64,
            limit: usize,
        ) -> Result<RoomEventPage, String> {
            self.record("events_before", &room_id).await;
            self.repository
                .events_before(&room_id, before_sequence, limit)
                .map_err(|error| error.to_string())
        }

        #[allow(clippy::too_many_arguments)]
        async fn post_message_checked(
            &self,
            room_id: String,
            recipients: Vec<MemberAddress>,
            content: String,
            mode: RoomInputMode,
            thread_key: String,
            expected_room_version: u64,
            command_id: String,
            reply_to_event_id: Option<String>,
        ) -> Result<PostMessageResult, String> {
            self.record("post_message", &room_id).await;
            self.repository
                .post_group_message_checked_with_reply(
                    &CollaborationActor::local(),
                    &room_id,
                    &recipients,
                    &content,
                    mode,
                    &thread_key,
                    expected_room_version,
                    &command_id,
                    reply_to_event_id.as_deref(),
                )
                .map_err(|error| error.to_string())
        }
    }

    fn handled_room_dispatch(
        dispatch: RoomProtocolDispatch,
    ) -> (Vec<WebProgressEvent>, Option<RoomOperationErrorIdentity>) {
        match dispatch {
            RoomProtocolDispatch::Handled {
                events,
                error_identity,
            } => (events, error_identity),
            RoomProtocolDispatch::Unhandled(message) => {
                panic!("expected handled room protocol message, got {message:?}")
            }
        }
    }

    fn handled_room_events(dispatch: RoomProtocolDispatch) -> Vec<WebProgressEvent> {
        handled_room_dispatch(dispatch).0
    }

    #[test]
    fn room_operation_error_payloads_include_complete_correlation_identity() {
        let cases = [
            (
                RoomOperationErrorIdentity::Post {
                    room_id: "room-a".into(),
                    command_id: "command-1".into(),
                },
                serde_json::json!({
                    "type": "error",
                    "message": "failed",
                    "room_operation": "post",
                    "room_id": "room-a",
                    "command_id": "command-1",
                }),
            ),
            (
                RoomOperationErrorIdentity::Directory {
                    room_id: "room-a".into(),
                    expected_room_version: 3,
                    working_directory: "D:\\workspace\\next".into(),
                },
                serde_json::json!({
                    "type": "error",
                    "message": "failed",
                    "room_operation": "directory",
                    "room_id": "room-a",
                    "expected_room_version": 3,
                    "working_directory": "D:\\workspace\\next",
                }),
            ),
            (
                RoomOperationErrorIdentity::Pagination {
                    room_id: "room-a".into(),
                    before_sequence: 10,
                },
                serde_json::json!({
                    "type": "error",
                    "message": "failed",
                    "room_operation": "pagination",
                    "room_id": "room-a",
                    "before_sequence": 10,
                }),
            ),
        ];

        for (identity, expected) in cases {
            assert_eq!(room_operation_error_json("failed", &identity), expected);
        }
    }

    #[test]
    fn room_operation_error_identity_is_consumed_by_only_the_primary_error() {
        let expected = RoomOperationErrorIdentity::Directory {
            room_id: "room-a".into(),
            expected_room_version: 3,
            working_directory: "D:\\workspace\\next".into(),
        };
        let mut identity = Some(expected.clone());
        let primary = WebProgressEvent::Error {
            message: "目录版本冲突".into(),
        };
        let refresh = WebProgressEvent::Error {
            message: "刷新协作房间快照失败".into(),
        };

        assert_eq!(
            take_room_operation_error_identity(&primary, &mut identity),
            Some(expected)
        );
        assert_eq!(
            take_room_operation_error_identity(&refresh, &mut identity),
            None
        );
    }

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
    fn collaboration_web_assets_expose_directory_reply_and_pagination_controls() {
        let html = include_str!("static/index.html");
        let script = include_str!("static/app.js");
        let reply_module = include_str!("static/room_reply.js");
        let style = include_str!("static/style.css");
        let collaboration_scripts = format!("{script}\n{reply_module}");

        for required_id in [
            "room-working-directory",
            "room-directory-modal",
            "load-earlier-events",
            "reply-preview",
            "reply-cancel",
        ] {
            assert!(
                html.contains(&format!("id=\"{required_id}\"")),
                "页面缺少 #{required_id}"
            );
        }
        let room_reply_position = html.find("src=\"/room_reply.js\"").unwrap();
        let app_position = html.find("src=\"/app.js\"").unwrap();
        assert!(room_reply_position < app_position);
        assert!(script.contains("operationResult.directoryConfirmed"));
        assert!(script.contains("RoomReply.captureTimelineViewport"));
        assert!(script.contains("RoomReply.timelineScrollTarget"));
        assert!(!script.contains("authoritativeRoomEventSequence = Math.max"));

        let progress_handler = script
            .split_once("function handleMemberRunProgress")
            .and_then(|(_, rest)| rest.split_once("function handleMemberRunFinished"))
            .map(|(handler, _)| handler)
            .expect("脚本缺少成员进度处理函数");
        assert!(progress_handler.contains("$messages.scrollTop = $messages.scrollHeight"));
        assert!(!progress_handler.contains("scrollToBottom()"));
        assert!(script.contains("'follow-if-near-bottom'"));

        let input_state_handler = script
            .split_once("function setInputEnabled")
            .and_then(|(_, rest)| rest.split_once("function updateSendButton"))
            .map(|(handler, _)| handler)
            .expect("脚本缺少输入状态处理函数");
        assert!(input_state_handler.contains("RoomReply.shouldFocusComposer"));
        assert!(input_state_handler.contains(".modal[aria-modal=\"true\"]:not(.hidden)"));

        let css_rule = |selector: &str| {
            let after_selector = style
                .split_once(selector)
                .unwrap_or_else(|| panic!("CSS 缺少 {selector}"))
                .1;
            after_selector
                .split_once('{')
                .and_then(|(_, body)| body.split_once('}').map(|(rule, _)| rule))
                .unwrap_or_else(|| panic!("CSS {selector} 规则不完整"))
        };
        assert!(css_rule(".reply-preview strong").contains("overflow-wrap: anywhere"));
        assert!(css_rule(".room-reply-reference strong").contains("overflow-wrap: anywhere"));

        for contract in [
            "RoomReply.beginReply",
            "RoomReply.buildRoomPostPayload",
            "reply_to_event_id",
            "room_message_accepted",
            "room_events_loaded_before",
            "update_room_working_directory",
            "load_room_events_before",
        ] {
            assert!(
                collaboration_scripts.contains(contract),
                "脚本缺少 {contract} 接线"
            );
        }
    }

    #[test]
    fn successful_retry_releases_the_legacy_query_binding() {
        let source = include_str!("ws_handler.rs");

        let retry_branch = source
            .split("ClientMessage::RetryLastUserMessage { message_id } =>")
            .nth(1)
            .unwrap()
            .split("ClientMessage::Cancel =>")
            .next()
            .unwrap();
        assert!(retry_branch.contains("Ok(snapshot) => {\n                                                    pending_legacy_query = None;"));
    }

    #[test]
    fn collaboration_client_messages_deserialize_with_explicit_targets() {
        let legacy: ClientMessage = serde_json::from_str(
            r#"{"type":"post_room_message","recipients":[],"content":"x","mode":"chat","expected_room_version":1}"#,
        )
        .unwrap();
        assert!(matches!(
            legacy,
            ClientMessage::PostRoomMessage {
                reply_to_event_id: None,
                ..
            }
        ));

        let reply: ClientMessage = serde_json::from_str(
            r#"{"type":"post_room_message","recipients":[],"content":"x","mode":"chat","expected_room_version":1,"reply_to_event_id":"event-9"}"#,
        )
        .unwrap();
        assert!(matches!(
            reply,
            ClientMessage::PostRoomMessage {
                reply_to_event_id: Some(id),
                ..
            } if id == "event-9"
        ));

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
                reply_to_event_id,
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
                assert_eq!(reply_to_event_id, None);
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

        let directory: ClientMessage = serde_json::from_str(
            r#"{"type":"update_room_working_directory","working_directory":"workspace-a","expected_room_version":12,"room_id":"forged-room"}"#,
        )
        .unwrap();
        assert!(matches!(
            directory,
            ClientMessage::UpdateRoomWorkingDirectory {
                working_directory,
                expected_room_version,
            } if working_directory == "workspace-a" && expected_room_version == 12
        ));

        let page: ClientMessage = serde_json::from_str(
            r#"{"type":"load_room_events_before","before_sequence":91,"room_id":"forged-room"}"#,
        )
        .unwrap();
        assert!(matches!(
            page,
            ClientMessage::LoadRoomEventsBefore {
                before_sequence,
                limit,
            } if before_sequence == 91 && limit == 100
        ));
    }

    #[test]
    fn collaboration_event_room_id_filters_new_protocol_events() {
        let loaded = WebProgressEvent::RoomEventsLoadedBefore {
            room_id: "room-loaded".into(),
            before_sequence: 42,
            has_more: false,
            events: Vec::new(),
        };
        let accepted = WebProgressEvent::RoomMessageAccepted {
            room_id: "room-accepted".into(),
            command_id: "command-1".into(),
            event_id: "event-1".into(),
            duplicate: false,
        };

        assert_eq!(collaboration_event_room_id(&loaded), Some("room-loaded"));
        assert_eq!(
            collaboration_event_room_id(&accepted),
            Some("room-accepted")
        );
        assert_eq!(
            serde_json::to_value(&loaded).unwrap()["type"],
            "room_events_loaded_before"
        );
        assert_eq!(
            serde_json::to_value(&accepted).unwrap()["type"],
            "room_message_accepted"
        );
    }

    #[tokio::test]
    async fn room_working_directory_websocket_uses_only_the_active_room() {
        let runtime_directory = tempfile::tempdir().unwrap();
        let startup_directory = tempfile::tempdir().unwrap();
        let requested_directory = tempfile::tempdir().unwrap();
        let repository = Arc::new(
            crate::web::collaboration::CollaborationRepository::new_with_startup_working_directory(
                runtime_directory.path(),
                CollaborationConfig::default(),
                startup_directory.path(),
            )
            .unwrap(),
        );
        let active = repository
            .ensure_room("active-room", "Active", &[])
            .unwrap();
        repository
            .ensure_room("forged-room", "Forged", &[])
            .unwrap();
        let protocol = TestRoomProtocol::new(Arc::clone(&repository));
        let message: ClientMessage = serde_json::from_value(serde_json::json!({
            "type": "update_room_working_directory",
            "room_id": "forged-room",
            "working_directory": requested_directory.path(),
            "expected_room_version": active.room.version,
        }))
        .unwrap();

        let events = handled_room_events(
            dispatch_room_protocol_message(&protocol, "active-room".into(), message).await,
        );

        assert!(events.is_empty(), "成功目录更新由 runtime 广播快照");
        assert_eq!(
            protocol.calls.lock().await.as_slice(),
            &[("update_directory".into(), "active-room".into())]
        );
        assert_eq!(
            std::path::PathBuf::from(
                repository
                    .snapshot("active-room")
                    .unwrap()
                    .room
                    .working_directory
            ),
            requested_directory.path().canonicalize().unwrap()
        );
        assert_eq!(
            std::path::PathBuf::from(
                repository
                    .snapshot("forged-room")
                    .unwrap()
                    .room
                    .working_directory
            ),
            startup_directory.path().canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn room_working_directory_websocket_stale_version_returns_error_then_fresh_snapshot() {
        let runtime_directory = tempfile::tempdir().unwrap();
        let startup_directory = tempfile::tempdir().unwrap();
        let authoritative_directory = tempfile::tempdir().unwrap();
        let rejected_directory = tempfile::tempdir().unwrap();
        let repository = Arc::new(
            crate::web::collaboration::CollaborationRepository::new_with_startup_working_directory(
                runtime_directory.path(),
                CollaborationConfig::default(),
                startup_directory.path(),
            )
            .unwrap(),
        );
        let initial = repository
            .ensure_room("active-room", "Active", &[])
            .unwrap();
        repository
            .update_room_working_directory(
                "active-room",
                &authoritative_directory.path().display().to_string(),
                initial.room.version,
            )
            .unwrap();
        let authoritative_path = authoritative_directory.path().canonicalize().unwrap();
        let protocol = TestRoomProtocol::new(Arc::clone(&repository));

        let (events, error_identity) = handled_room_dispatch(
            dispatch_room_protocol_message(
                &protocol,
                "active-room".into(),
                ClientMessage::UpdateRoomWorkingDirectory {
                    working_directory: rejected_directory.path().display().to_string(),
                    expected_room_version: initial.room.version,
                },
            )
            .await,
        );

        assert_eq!(
            error_identity,
            Some(RoomOperationErrorIdentity::Directory {
                room_id: "active-room".into(),
                expected_room_version: initial.room.version,
                working_directory: rejected_directory.path().display().to_string(),
            })
        );
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            WebProgressEvent::Error { message } if message.contains("版本冲突")
        ));
        assert!(matches!(
            &events[1],
            WebProgressEvent::RoomSnapshot { snapshot }
                if std::path::Path::new(&snapshot.room.working_directory)
                    == authoritative_path.as_path()
        ));
        assert_eq!(
            protocol.calls.lock().await.as_slice(),
            &[
                ("update_directory".into(), "active-room".into()),
                ("snapshot".into(), "active-room".into()),
            ]
        );
    }

    #[tokio::test]
    async fn room_working_directory_websocket_non_version_error_never_refreshes_snapshot() {
        let runtime_directory = tempfile::tempdir().unwrap();
        let startup_directory = tempfile::tempdir().unwrap();
        let repository = Arc::new(
            crate::web::collaboration::CollaborationRepository::new_with_startup_working_directory(
                runtime_directory.path(),
                CollaborationConfig::default(),
                startup_directory.path(),
            )
            .unwrap(),
        );
        let initial = repository
            .ensure_room("active-room", "Active", &[])
            .unwrap();
        let non_directory = startup_directory.path().join("版本冲突.txt");
        std::fs::write(&non_directory, b"not a directory").unwrap();
        let protocol = TestRoomProtocol::new(repository);

        let (events, error_identity) = handled_room_dispatch(
            dispatch_room_protocol_message(
                &protocol,
                "active-room".into(),
                ClientMessage::UpdateRoomWorkingDirectory {
                    working_directory: non_directory.display().to_string(),
                    expected_room_version: initial.room.version,
                },
            )
            .await,
        );

        assert_eq!(
            error_identity,
            Some(RoomOperationErrorIdentity::Directory {
                room_id: "active-room".into(),
                expected_room_version: initial.room.version,
                working_directory: non_directory.display().to_string(),
            })
        );
        assert!(matches!(
            events.as_slice(),
            [WebProgressEvent::Error { message }]
                if message.contains("房间工作目录不是目录") && message.contains("版本冲突.txt")
        ));
        assert_eq!(
            protocol.calls.lock().await.as_slice(),
            &[("update_directory".into(), "active-room".into())]
        );
    }

    #[tokio::test]
    async fn room_events_before_websocket_reads_only_active_room_and_clamps_limit() {
        let runtime_directory = tempfile::tempdir().unwrap();
        let startup_directory = tempfile::tempdir().unwrap();
        let repository = Arc::new(
            crate::web::collaboration::CollaborationRepository::new_with_startup_working_directory(
                runtime_directory.path(),
                CollaborationConfig::default(),
                startup_directory.path(),
            )
            .unwrap(),
        );
        let now = chrono::Utc::now();
        let history = (1..=150)
            .map(|index| LegacyMessageSeed {
                id: format!("message-{index}"),
                role: "assistant".into(),
                content: format!("历史 {index}"),
                timestamp: now + chrono::Duration::seconds(index),
                hidden: false,
            })
            .collect::<Vec<_>>();
        let active = repository
            .ensure_room("active-room", "Active", &history)
            .unwrap();
        repository
            .ensure_room("forged-room", "Forged", &[])
            .unwrap();
        let protocol = TestRoomProtocol::new(repository);
        let message: ClientMessage = serde_json::from_value(serde_json::json!({
            "type": "load_room_events_before",
            "room_id": "forged-room",
            "before_sequence": active.room.latest_event_seq + 1,
            "limit": usize::MAX,
        }))
        .unwrap();

        let events = handled_room_events(
            dispatch_room_protocol_message(&protocol, "active-room".into(), message).await,
        );

        assert!(matches!(
            events.as_slice(),
            [WebProgressEvent::RoomEventsLoadedBefore {
                room_id,
                before_sequence,
                has_more: true,
                events,
            }] if room_id == "active-room"
                && *before_sequence == active.room.latest_event_seq + 1
                && events.len() == 100
                && events.iter().all(|event| event.room_id == "active-room")
        ));
        assert_eq!(
            protocol.calls.lock().await.as_slice(),
            &[("events_before".into(), "active-room".into())]
        );

        let (failed_events, error_identity) = handled_room_dispatch(
            dispatch_room_protocol_message(
                &protocol,
                "missing-room".into(),
                ClientMessage::LoadRoomEventsBefore {
                    before_sequence: 42,
                    limit: 100,
                },
            )
            .await,
        );
        assert!(matches!(
            failed_events.as_slice(),
            [WebProgressEvent::Error { .. }]
        ));
        assert_eq!(
            error_identity,
            Some(RoomOperationErrorIdentity::Pagination {
                room_id: "missing-room".into(),
                before_sequence: 42,
            })
        );
    }

    #[tokio::test]
    async fn room_message_ack_websocket_accepts_valid_reply_and_rejects_invalid_reply() {
        let runtime_directory = tempfile::tempdir().unwrap();
        let startup_directory = tempfile::tempdir().unwrap();
        let repository = Arc::new(
            crate::web::collaboration::CollaborationRepository::new_with_startup_working_directory(
                runtime_directory.path(),
                CollaborationConfig::default(),
                startup_directory.path(),
            )
            .unwrap(),
        );
        let target_id = "legacy-active-room-target";
        let initial = repository
            .ensure_room(
                "active-room",
                "Active",
                &[LegacyMessageSeed {
                    id: "target".into(),
                    role: "user".into(),
                    content: "被引用消息".into(),
                    timestamp: chrono::Utc::now(),
                    hidden: false,
                }],
            )
            .unwrap();
        let recipient = MemberAddress {
            member_id: initial.room.default_member_id.clone(),
            expected_version: initial.members[0].version,
        };
        let protocol = TestRoomProtocol::new(Arc::clone(&repository));
        let valid_message = || ClientMessage::PostRoomMessage {
            recipients: vec![recipient.clone()],
            content: "带引用的新消息".into(),
            mode: RoomInputMode::Chat,
            thread_key: DEFAULT_THREAD_KEY.into(),
            expected_room_version: initial.room.version,
            command_id: "command-valid".into(),
            reply_to_event_id: Some(target_id.into()),
        };

        let accepted = handled_room_events(
            dispatch_room_protocol_message(&protocol, "active-room".into(), valid_message()).await,
        );
        let accepted_event_id = match accepted.as_slice() {
            [WebProgressEvent::RoomMessageAccepted {
                room_id,
                command_id,
                event_id,
                duplicate: false,
            }] if room_id == "active-room" && command_id == "command-valid" => event_id.clone(),
            other => panic!("expected accepted reply, got {other:?}"),
        };
        let stored = repository
            .snapshot("active-room")
            .unwrap()
            .events
            .into_iter()
            .find(|event| event.event_id == accepted_event_id)
            .unwrap();
        assert_eq!(stored.parent_event_id.as_deref(), Some(target_id));

        let duplicate = handled_room_events(
            dispatch_room_protocol_message(&protocol, "active-room".into(), valid_message()).await,
        );
        assert!(matches!(
            duplicate.as_slice(),
            [WebProgressEvent::RoomMessageAccepted {
                room_id,
                command_id,
                event_id,
                duplicate: true,
            }] if room_id == "active-room"
                && command_id == "command-valid"
                && event_id == &accepted_event_id
        ));

        let current = repository.snapshot("active-room").unwrap();
        let (rejected, error_identity) = handled_room_dispatch(
            dispatch_room_protocol_message(
                &protocol,
                "active-room".into(),
                ClientMessage::PostRoomMessage {
                    recipients: vec![MemberAddress {
                        member_id: current.room.default_member_id.clone(),
                        expected_version: current.members[0].version,
                    }],
                    content: "非法引用".into(),
                    mode: RoomInputMode::Chat,
                    thread_key: DEFAULT_THREAD_KEY.into(),
                    expected_room_version: current.room.version,
                    command_id: "command-invalid".into(),
                    reply_to_event_id: Some("missing-event".into()),
                },
            )
            .await,
        );
        assert_eq!(
            error_identity,
            Some(RoomOperationErrorIdentity::Post {
                room_id: "active-room".into(),
                command_id: "command-invalid".into(),
            })
        );
        assert!(matches!(
            rejected.as_slice(),
            [WebProgressEvent::Error { message }] if message.contains("回复目标不存在")
        ));
        assert!(!rejected
            .iter()
            .any(|event| matches!(event, WebProgressEvent::RoomMessageAccepted { .. })));
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
