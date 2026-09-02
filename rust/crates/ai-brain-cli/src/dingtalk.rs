//! Native DingTalk Stream transport for the durable collaboration runtime.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::time::{interval, sleep, timeout, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::orchestrator::Orchestrator;
use crate::web::collaboration::{InboxState, RoomInputMode};
use crate::web::collaboration_app::CollaborationApplication;
use crate::web::collaboration_runtime::CollaborationRuntime;

const CHATBOT_TOPIC: &str = "/v1.0/im/bot/messages/get";
const OPEN_CONNECTION_URL: &str = "https://api.dingtalk.com/v1.0/gateway/connections/open";
const MAX_INPUT_CHARS: usize = 20_000;
const REPLY_CHUNK_CHARS: usize = 4_000;
const DEFAULT_REPLY_TIMEOUT_SECONDS: u64 = 30 * 60;
const DEFAULT_RECONNECT_SECONDS: u64 = 5;

#[derive(Clone)]
struct DingTalkSettings {
    client_id: String,
    client_secret: String,
    allowed_staff_ids: HashSet<String>,
    reply_timeout: Duration,
    reconnect_delay: Duration,
}

impl DingTalkSettings {
    fn from_environment() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Result<Self, String> {
        let client_id = required_setting("DINGTALK_CLIENT_ID", &mut lookup)?;
        let client_secret = required_setting("DINGTALK_CLIENT_SECRET", &mut lookup)?;
        let allowed_staff_ids = lookup("DINGTALK_ALLOWED_STAFF_IDS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();
        let reply_timeout = duration_setting(
            "DINGTALK_REPLY_TIMEOUT_SECONDS",
            DEFAULT_REPLY_TIMEOUT_SECONDS,
            &mut lookup,
        )?;
        let reconnect_delay = duration_setting(
            "DINGTALK_RECONNECT_SECONDS",
            DEFAULT_RECONNECT_SECONDS,
            &mut lookup,
        )?;
        Ok(Self {
            client_id,
            client_secret,
            allowed_staff_ids,
            reply_timeout,
            reconnect_delay,
        })
    }
}

fn required_setting(
    name: &str,
    lookup: &mut impl FnMut(&str) -> Option<String>,
) -> Result<String, String> {
    let value = lookup(name).unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        Err(format!("缺少环境变量 {name}"))
    } else {
        Ok(value.to_owned())
    }
}

fn duration_setting(
    name: &str,
    default_seconds: u64,
    lookup: &mut impl FnMut(&str) -> Option<String>,
) -> Result<Duration, String> {
    let Some(raw) = lookup(name) else {
        return Ok(Duration::from_secs(default_seconds));
    };
    let seconds = raw
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("环境变量 {name} 必须是正整数秒数"))?;
    if seconds == 0 {
        return Err(format!("环境变量 {name} 必须大于 0"));
    }
    Ok(Duration::from_secs(seconds))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenConnectionRequest<'a> {
    client_id: &'a str,
    client_secret: &'a str,
    subscriptions: [Subscription<'a>; 1],
    ua: &'a str,
    local_ip: &'a str,
}

#[derive(Debug, Serialize)]
struct Subscription<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    topic: &'a str,
}

#[derive(Deserialize)]
struct OpenConnectionResponse {
    endpoint: String,
    ticket: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamEnvelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    headers: StreamHeaders,
    #[serde(default)]
    data: Value,
}

impl StreamEnvelope {
    fn parsed_data(&self) -> Result<Value, String> {
        match &self.data {
            Value::String(value) => serde_json::from_str(value)
                .map_err(|error| format!("解析 Stream data 失败: {error}")),
            value @ Value::Object(_) => Ok(value.clone()),
            Value::Null => Ok(Value::Object(serde_json::Map::new())),
            _ => Err("Stream data 必须是 JSON 字符串或对象".into()),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamHeaders {
    #[serde(default)]
    message_id: String,
    #[serde(default)]
    topic: String,
}

#[derive(Debug, Serialize)]
struct AckFrame {
    code: u16,
    headers: AckHeaders,
    message: String,
    data: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AckHeaders {
    message_id: String,
    content_type: &'static str,
}

impl AckFrame {
    fn callback(message_id: &str, response: &str) -> Self {
        Self {
            code: 200,
            headers: AckHeaders {
                message_id: message_id.to_owned(),
                content_type: "application/json",
            },
            message: String::new(),
            data: json!({ "response": response }).to_string(),
        }
    }

    fn system(message_id: &str, data: &Value) -> Self {
        Self {
            code: 200,
            headers: AckHeaders {
                message_id: message_id.to_owned(),
                content_type: "application/json",
            },
            message: "OK".into(),
            data: data.to_string(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatbotPayload {
    #[serde(default)]
    msg_id: String,
    #[serde(default)]
    conversation_id: String,
    #[serde(default)]
    conversation_type: String,
    #[serde(default)]
    conversation_title: String,
    #[serde(default)]
    sender_staff_id: String,
    #[serde(default)]
    sender_id: String,
    #[serde(default)]
    sender_nick: String,
    #[serde(default)]
    robot_code: String,
    #[serde(default)]
    is_in_at_list: bool,
    #[serde(default)]
    session_webhook: String,
    #[serde(default)]
    session_webhook_expired_time: Option<i64>,
    #[serde(default)]
    msgtype: String,
    text: Option<TextPayload>,
}

#[derive(Deserialize)]
struct TextPayload {
    #[serde(default)]
    content: String,
}

#[derive(Clone)]
struct IncomingTextMessage {
    message_id: String,
    conversation_id: String,
    conversation_title: String,
    sender_id: String,
    sender_staff_id: Option<String>,
    sender_name: String,
    robot_code: String,
    content: String,
    session_webhook: Url,
    session_webhook_expired_time: Option<i64>,
}

enum CallbackDisposition {
    Ignore(&'static str),
    Reject {
        reason: &'static str,
        user_message: Option<&'static str>,
        webhook: Option<Url>,
        sender_staff_id: Option<String>,
    },
    Accept(IncomingTextMessage),
}

fn normalize_callback(payload: ChatbotPayload, settings: &DingTalkSettings) -> CallbackDisposition {
    if payload.conversation_type == "2" && !payload.is_in_at_list {
        return CallbackDisposition::Ignore("group_message_without_robot_mention");
    }

    let webhook = validate_session_webhook(&payload.session_webhook).ok();
    let sender_staff_id = non_empty(&payload.sender_staff_id);
    let sender_id = sender_staff_id
        .clone()
        .or_else(|| non_empty(&payload.sender_id));

    if !settings.allowed_staff_ids.is_empty()
        && sender_staff_id
            .as_ref()
            .is_none_or(|id| !settings.allowed_staff_ids.contains(id))
    {
        return CallbackDisposition::Ignore("sender_not_allowed");
    }
    if payload.msgtype != "text" {
        return CallbackDisposition::Reject {
            reason: "unsupported_message_type",
            user_message: Some("当前仅支持文本消息。"),
            webhook,
            sender_staff_id,
        };
    }
    let content = payload
        .text
        .map(|text| text.content.trim().to_owned())
        .unwrap_or_default();
    if content.is_empty() {
        return CallbackDisposition::Reject {
            reason: "empty_text",
            user_message: Some("消息内容不能为空。"),
            webhook,
            sender_staff_id,
        };
    }
    if content.chars().count() > MAX_INPUT_CHARS {
        return CallbackDisposition::Reject {
            reason: "text_too_long",
            user_message: Some("消息过长，请缩短后重试。"),
            webhook,
            sender_staff_id,
        };
    }
    let Some(sender_id) = sender_id else {
        return CallbackDisposition::Ignore("missing_sender_id");
    };
    if payload.msg_id.trim().is_empty() || payload.conversation_id.trim().is_empty() {
        return CallbackDisposition::Ignore("missing_message_or_conversation_id");
    }
    let Some(session_webhook) = webhook else {
        return CallbackDisposition::Ignore("invalid_session_webhook");
    };

    CallbackDisposition::Accept(IncomingTextMessage {
        message_id: payload.msg_id.trim().to_owned(),
        conversation_id: payload.conversation_id.trim().to_owned(),
        conversation_title: payload.conversation_title.trim().to_owned(),
        sender_id,
        sender_staff_id,
        sender_name: payload.sender_nick.trim().to_owned(),
        robot_code: non_empty(&payload.robot_code).unwrap_or_else(|| settings.client_id.clone()),
        content,
        session_webhook,
        session_webhook_expired_time: payload.session_webhook_expired_time,
    })
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn validate_session_webhook(value: &str) -> Result<Url, String> {
    let url = Url::parse(value.trim()).map_err(|_| "sessionWebhook 不是有效 URL".to_owned())?;
    let host = url
        .host_str()
        .ok_or_else(|| "sessionWebhook 缺少主机名".to_owned())?;
    if url.scheme() != "https"
        || !(host == "dingtalk.com" || host.ends_with(".dingtalk.com"))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("sessionWebhook 必须是无用户凭据的钉钉 HTTPS 地址".into());
    }
    Ok(url)
}

#[derive(Clone)]
struct CollaborationBridge {
    runtime: Arc<CollaborationRuntime>,
    reply_timeout: Duration,
}

struct Admission {
    room_id: String,
    root_event_id: String,
    after_sequence: u64,
    duplicate: bool,
}

impl CollaborationBridge {
    async fn admit(&self, message: &IncomingTextMessage) -> Result<Admission, String> {
        let room_id = room_id_for(&message.robot_code, &message.conversation_id);
        let title = room_title(message);
        let snapshot = self
            .runtime
            .ensure_room(room_id.clone(), title, Vec::new())
            .await?;
        let command_id = format!("dingtalk:{}:{}", message.robot_code, message.message_id);
        let result = self
            .runtime
            .post_message(
                room_id.clone(),
                vec![snapshot.room.default_member_id],
                message.content.clone(),
                RoomInputMode::Chat,
                command_id,
            )
            .await?;
        Ok(Admission {
            room_id,
            root_event_id: result.event.event_id,
            after_sequence: result.event.sequence,
            duplicate: result.duplicate,
        })
    }

    async fn wait_for_reply(&self, admission: &Admission) -> Result<String, String> {
        timeout(self.reply_timeout, async {
            let mut after_sequence = admission.after_sequence;
            loop {
                let events = self
                    .runtime
                    .events_after(admission.room_id.clone(), after_sequence)
                    .await?;
                for event in events {
                    after_sequence = after_sequence.max(event.sequence);
                    if event.kind == "member_message"
                        && event.conversation_root_event_id == admission.root_event_id
                    {
                        return Ok(event.content);
                    }
                }

                let snapshot = self.runtime.snapshot(admission.room_id.clone()).await?;
                if let Some(item) = snapshot.inbox.iter().find(|item| {
                    item.source_event_id == admission.root_event_id
                        && matches!(item.state, InboxState::Failed | InboxState::Cancelled)
                }) {
                    return Err(item
                        .error
                        .clone()
                        .unwrap_or_else(|| "智脑任务未能完成".into()));
                }
                sleep(Duration::from_millis(400)).await;
            }
        })
        .await
        .map_err(|_| "等待智脑回复超时".to_owned())?
    }
}

fn room_id_for(robot_code: &str, conversation_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(robot_code.as_bytes());
    hasher.update([0]);
    hasher.update(conversation_id.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("dingtalk-{}", &digest[..24])
}

fn room_title(message: &IncomingTextMessage) -> String {
    let raw = if !message.conversation_title.is_empty() {
        format!("钉钉 · {}", message.conversation_title)
    } else if !message.sender_name.is_empty() {
        format!("钉钉 · {}", message.sender_name)
    } else {
        format!("钉钉 · {}", message.sender_id)
    };
    raw.chars().take(80).collect()
}

#[derive(Clone)]
struct DingTalkTransport {
    settings: DingTalkSettings,
    bridge: CollaborationBridge,
    http: reqwest::Client,
}

impl DingTalkTransport {
    fn new(settings: DingTalkSettings, runtime: Arc<CollaborationRuntime>) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| format!("创建钉钉 HTTP 客户端失败: {error}"))?;
        Ok(Self {
            bridge: CollaborationBridge {
                runtime,
                reply_timeout: settings.reply_timeout,
            },
            settings,
            http,
        })
    }

    async fn run_forever(self) {
        loop {
            if let Err(error) = self.run_connection().await {
                tracing::warn!(error = %error, "钉钉 Stream 连接中断");
            }
            sleep(self.settings.reconnect_delay).await;
        }
    }

    async fn run_connection(&self) -> Result<(), String> {
        let websocket_url = self.open_connection().await?;
        let endpoint_host = websocket_url.host_str().unwrap_or("unknown").to_owned();
        let (websocket, _) = tokio_tungstenite::connect_async(websocket_url.as_str())
            .await
            .map_err(|error| format!("连接钉钉 Stream WebSocket 失败: {error}"))?;
        tracing::info!(endpoint_host, "钉钉 Stream 已连接");

        let (mut writer, mut reader) = websocket.split();
        let mut keepalive = interval(Duration::from_secs(60));
        keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
        keepalive.tick().await;

        loop {
            tokio::select! {
                _ = keepalive.tick() => {
                    writer.send(Message::Ping(Vec::new().into())).await
                        .map_err(|error| format!("发送钉钉 Stream 心跳失败: {error}"))?;
                }
                next = reader.next() => {
                    let Some(frame) = next else {
                        return Err("钉钉 Stream WebSocket 已关闭".into());
                    };
                    match frame.map_err(|error| format!("读取钉钉 Stream 帧失败: {error}"))? {
                        Message::Text(text) => {
                            if self.handle_text_frame(text.as_str(), &mut writer).await? {
                                return Ok(());
                            }
                        }
                        Message::Ping(payload) => {
                            writer.send(Message::Pong(payload)).await
                                .map_err(|error| format!("回复钉钉 Stream 心跳失败: {error}"))?;
                        }
                        Message::Close(_) => return Ok(()),
                        Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
                    }
                }
            }
        }
    }

    async fn open_connection(&self) -> Result<Url, String> {
        let local_ip = local_ip();
        let request = OpenConnectionRequest {
            client_id: &self.settings.client_id,
            client_secret: &self.settings.client_secret,
            subscriptions: [Subscription {
                kind: "CALLBACK",
                topic: CHATBOT_TOPIC,
            }],
            ua: concat!("ai-brain-rust/", env!("CARGO_PKG_VERSION")),
            local_ip: &local_ip,
        };
        let response = self
            .http
            .post(OPEN_CONNECTION_URL)
            .header("Accept", "application/json")
            .header(
                reqwest::header::USER_AGENT,
                concat!("DingTalkStream/1.0 AI-Brain/", env!("CARGO_PKG_VERSION")),
            )
            .json(&request)
            .send()
            .await
            .map_err(|error| format!("获取钉钉 Stream 连接票据失败: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "获取钉钉 Stream 连接票据失败: HTTP {}",
                response.status()
            ));
        }
        let connection: OpenConnectionResponse = response
            .json()
            .await
            .map_err(|error| format!("解析钉钉 Stream 连接票据失败: {error}"))?;
        let mut endpoint = Url::parse(&connection.endpoint)
            .map_err(|error| format!("钉钉 Stream endpoint 无效: {error}"))?;
        let endpoint_host = endpoint.host_str().unwrap_or_default();
        if endpoint.scheme() != "wss"
            || !(endpoint_host == "dingtalk.com" || endpoint_host.ends_with(".dingtalk.com"))
            || connection.ticket.trim().is_empty()
        {
            return Err("钉钉 Stream endpoint 或 ticket 无效".into());
        }
        endpoint
            .query_pairs_mut()
            .append_pair("ticket", &connection.ticket);
        Ok(endpoint)
    }

    async fn handle_text_frame<S>(&self, text: &str, writer: &mut S) -> Result<bool, String>
    where
        S: futures_util::Sink<Message> + Unpin,
        S::Error: std::fmt::Display,
    {
        let envelope: StreamEnvelope = serde_json::from_str(text)
            .map_err(|error| format!("解析钉钉 Stream 帧失败: {error}"))?;
        match envelope.kind.as_str() {
            "CALLBACK" if envelope.headers.topic == CHATBOT_TOPIC => {
                let data = envelope.parsed_data()?;
                let payload: ChatbotPayload = serde_json::from_value(data)
                    .map_err(|error| format!("解析钉钉机器人消息失败: {error}"))?;
                self.handle_chatbot_callback(payload).await;
                send_ack(
                    writer,
                    AckFrame::callback(&envelope.headers.message_id, "OK"),
                )
                .await?;
                Ok(false)
            }
            "SYSTEM" => {
                let data = envelope.parsed_data()?;
                send_ack(
                    writer,
                    AckFrame::system(&envelope.headers.message_id, &data),
                )
                .await?;
                Ok(envelope.headers.topic == "disconnect")
            }
            _ => {
                tracing::warn!(
                    frame_type = %envelope.kind,
                    topic = %envelope.headers.topic,
                    "忽略未订阅的钉钉 Stream 帧"
                );
                Ok(false)
            }
        }
    }

    async fn handle_chatbot_callback(&self, payload: ChatbotPayload) {
        match normalize_callback(payload, &self.settings) {
            CallbackDisposition::Ignore(reason) => {
                tracing::info!(reason, "忽略钉钉机器人消息");
            }
            CallbackDisposition::Reject {
                reason,
                user_message,
                webhook,
                sender_staff_id,
            } => {
                tracing::info!(reason, "拒绝钉钉机器人消息");
                if let (Some(user_message), Some(webhook)) = (user_message, webhook) {
                    let transport = self.clone();
                    tokio::spawn(async move {
                        if let Err(error) = transport
                            .send_text_reply(&webhook, user_message, sender_staff_id.as_deref())
                            .await
                        {
                            tracing::warn!(error = %error, "发送钉钉输入错误提示失败");
                        }
                    });
                }
            }
            CallbackDisposition::Accept(message) => {
                let message_hash = short_hash(&message.message_id);
                let conversation_hash = short_hash(&message.conversation_id);
                match self.bridge.admit(&message).await {
                    Ok(admission) if admission.duplicate => {
                        tracing::info!(message_hash, conversation_hash, "忽略重复的钉钉消息");
                    }
                    Ok(admission) => {
                        tracing::info!(
                            message_hash,
                            conversation_hash,
                            room_id = %admission.room_id,
                            "钉钉消息已进入智脑"
                        );
                        let transport = self.clone();
                        tokio::spawn(async move {
                            transport.process_admitted_message(message, admission).await;
                        });
                    }
                    Err(error) => {
                        tracing::error!(
                            message_hash,
                            conversation_hash,
                            error = %error,
                            "钉钉消息写入智脑失败"
                        );
                        let transport = self.clone();
                        tokio::spawn(async move {
                            transport
                                .send_failure_reply(&message, "智脑暂时无法接收消息，请稍后重试。")
                                .await;
                        });
                    }
                }
            }
        }
    }

    async fn process_admitted_message(&self, message: IncomingTextMessage, admission: Admission) {
        let message_hash = short_hash(&message.message_id);
        let conversation_hash = short_hash(&message.conversation_id);
        let reply = match self.bridge.wait_for_reply(&admission).await {
            Ok(reply) => reply,
            Err(error) => {
                tracing::error!(message_hash, conversation_hash, error = %error, "智脑处理钉钉消息失败");
                "智脑处理失败，请稍后重试。".into()
            }
        };
        if webhook_expired(message.session_webhook_expired_time) {
            tracing::warn!(
                message_hash,
                conversation_hash,
                "钉钉会话 Webhook 已过期，无法回发结果"
            );
            return;
        }
        if let Err(error) = self
            .send_text_reply(
                &message.session_webhook,
                &reply,
                message.sender_staff_id.as_deref(),
            )
            .await
        {
            tracing::error!(message_hash, conversation_hash, error = %error, "回发钉钉消息失败");
        }
    }

    async fn send_failure_reply(&self, message: &IncomingTextMessage, text: &str) {
        if let Err(error) = self
            .send_text_reply(
                &message.session_webhook,
                text,
                message.sender_staff_id.as_deref(),
            )
            .await
        {
            tracing::warn!(error = %error, "回发钉钉失败提示失败");
        }
    }

    async fn send_text_reply(
        &self,
        webhook: &Url,
        text: &str,
        sender_staff_id: Option<&str>,
    ) -> Result<(), String> {
        for chunk in split_text(text, REPLY_CHUNK_CHARS) {
            let at_user_ids = sender_staff_id.into_iter().collect::<Vec<_>>();
            let response = self
                .http
                .post(webhook.clone())
                .header("Accept", "application/json")
                .json(&json!({
                    "msgtype": "text",
                    "text": { "content": chunk },
                    "at": { "atUserIds": at_user_ids },
                }))
                .send()
                .await
                .map_err(|error| format!("调用钉钉会话 Webhook 失败: {error}"))?;
            let status = response.status();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            if !status.is_success() {
                return Err(format!("钉钉会话 Webhook 返回 HTTP {status}"));
            }
            if body
                .get("errcode")
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
            {
                return Err(format!(
                    "钉钉会话 Webhook 拒绝消息: {}",
                    body.get("errmsg")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                ));
            }
        }
        Ok(())
    }
}

async fn send_ack<S>(writer: &mut S, ack: AckFrame) -> Result<(), String>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let payload = serde_json::to_string(&ack)
        .map_err(|error| format!("序列化钉钉 Stream ACK 失败: {error}"))?;
    writer
        .send(Message::Text(payload.into()))
        .await
        .map_err(|error| format!("发送钉钉 Stream ACK 失败: {error}"))
}

fn webhook_expired(expired_time: Option<i64>) -> bool {
    let Some(expired_time) = expired_time else {
        return false;
    };
    let now_millis = chrono::Utc::now().timestamp_millis();
    let normalized = if expired_time < 10_000_000_000 {
        expired_time.saturating_mul(1_000)
    } else {
        expired_time
    };
    normalized <= now_millis
}

fn split_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return vec!["（智脑返回了空响应）".into()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0_usize;
    for character in text.chars() {
        if current_chars == max_chars {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push(character);
        current_chars += 1;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("8.8.8.8:80")?;
            socket.local_addr()
        })
        .map(|address| address.ip().to_string())
        .unwrap_or_default()
}

fn short_hash(value: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(value.as_bytes()));
    digest[..12].to_owned()
}

pub fn validate_environment() -> Result<(), String> {
    DingTalkSettings::from_environment().map(|_| ())
}

pub async fn run(orch: Orchestrator) -> Result<(), String> {
    let settings = DingTalkSettings::from_environment()?;
    if settings.allowed_staff_ids.is_empty() {
        tracing::warn!("DINGTALK_ALLOWED_STAFF_IDS 未配置，机器人将接受发布范围内所有用户");
    }
    let workspace_root =
        std::env::current_dir().map_err(|error| format!("读取钉钉智脑工作目录失败: {error}"))?;
    let application = CollaborationApplication::start(orch, &workspace_root).await?;
    let transport = DingTalkTransport::new(settings, application.runtime)?;
    tracing::info!("钉钉 Stream 传输已启动，按 Ctrl+C 停止");
    tokio::select! {
        () = transport.run_forever() => Ok(()),
        signal = tokio::signal::ctrl_c() => signal
            .map_err(|error| format!("监听退出信号失败: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn settings(values: &[(&str, &str)]) -> Result<DingTalkSettings, String> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        DingTalkSettings::from_lookup(|name| values.get(name).cloned())
    }

    fn base_settings() -> DingTalkSettings {
        settings(&[
            ("DINGTALK_CLIENT_ID", "client-id"),
            ("DINGTALK_CLIENT_SECRET", "client-secret"),
        ])
        .unwrap()
    }

    fn payload(conversation_type: &str, mentioned: bool) -> ChatbotPayload {
        ChatbotPayload {
            msg_id: "message-1".into(),
            conversation_id: "conversation-1".into(),
            conversation_type: conversation_type.into(),
            conversation_title: "研发群".into(),
            sender_staff_id: "staff-1".into(),
            sender_id: "encrypted-1".into(),
            sender_nick: "测试用户".into(),
            robot_code: "robot-1".into(),
            is_in_at_list: mentioned,
            session_webhook: "https://api.dingtalk.com/agent/send?session=secret".into(),
            session_webhook_expired_time: None,
            msgtype: "text".into(),
            text: Some(TextPayload {
                content: "  分析这个仓库  ".into(),
            }),
        }
    }

    #[test]
    fn settings_require_credentials_and_parse_allowlist() {
        let error = settings(&[]).err().unwrap();
        assert_eq!(error, "缺少环境变量 DINGTALK_CLIENT_ID");

        let parsed = settings(&[
            ("DINGTALK_CLIENT_ID", " id "),
            ("DINGTALK_CLIENT_SECRET", " secret "),
            ("DINGTALK_ALLOWED_STAFF_IDS", " staff-1, staff-2, "),
            ("DINGTALK_REPLY_TIMEOUT_SECONDS", "42"),
        ])
        .unwrap();
        assert_eq!(parsed.client_id, "id");
        assert_eq!(parsed.client_secret, "secret");
        assert_eq!(parsed.allowed_staff_ids.len(), 2);
        assert_eq!(parsed.reply_timeout, Duration::from_secs(42));
    }

    #[test]
    fn open_connection_request_matches_stream_subscription_contract() {
        let request = OpenConnectionRequest {
            client_id: "client-id",
            client_secret: "client-secret",
            subscriptions: [Subscription {
                kind: "CALLBACK",
                topic: CHATBOT_TOPIC,
            }],
            ua: "test",
            local_ip: "",
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["clientId"], "client-id");
        assert_eq!(value["subscriptions"][0]["type"], "CALLBACK");
        assert_eq!(value["subscriptions"][0]["topic"], CHATBOT_TOPIC);
    }

    #[test]
    fn callback_frame_parses_string_data_and_builds_correlated_ack() {
        let frame: StreamEnvelope = serde_json::from_value(json!({
            "specVersion": "1.0",
            "type": "CALLBACK",
            "headers": {
                "messageId": "stream-message-1",
                "topic": CHATBOT_TOPIC
            },
            "data": json!({
                "msgId": "chat-message-1",
                "conversationId": "conversation-1"
            }).to_string()
        }))
        .unwrap();
        assert_eq!(frame.parsed_data().unwrap()["msgId"], "chat-message-1");

        let ack =
            serde_json::to_value(AckFrame::callback(&frame.headers.message_id, "OK")).unwrap();
        assert_eq!(ack["code"], 200);
        assert_eq!(ack["headers"]["messageId"], "stream-message-1");
        assert_eq!(ack["data"], r#"{"response":"OK"}"#);
    }

    #[test]
    fn group_requires_mention_while_direct_message_is_accepted() {
        assert!(matches!(
            normalize_callback(payload("2", false), &base_settings()),
            CallbackDisposition::Ignore("group_message_without_robot_mention")
        ));
        let CallbackDisposition::Accept(message) =
            normalize_callback(payload("1", false), &base_settings())
        else {
            panic!("direct text should be accepted");
        };
        assert_eq!(message.content, "分析这个仓库");
        assert_eq!(message.sender_id, "staff-1");
    }

    #[test]
    fn allowlist_requires_the_published_staff_id() {
        let settings = settings(&[
            ("DINGTALK_CLIENT_ID", "client-id"),
            ("DINGTALK_CLIENT_SECRET", "client-secret"),
            ("DINGTALK_ALLOWED_STAFF_IDS", "staff-2"),
        ])
        .unwrap();
        assert!(matches!(
            normalize_callback(payload("1", false), &settings),
            CallbackDisposition::Ignore("sender_not_allowed")
        ));
    }

    #[test]
    fn room_ids_are_stable_and_robot_scoped() {
        let first = room_id_for("robot-1", "conversation-1");
        assert_eq!(first, room_id_for("robot-1", "conversation-1"));
        assert_ne!(first, room_id_for("robot-2", "conversation-1"));
        assert_ne!(first, room_id_for("robot-1", "conversation-2"));
        assert!(first.starts_with("dingtalk-"));
    }

    #[test]
    fn repeated_dingtalk_message_is_admitted_only_once() {
        use crate::web::collaboration::{CollaborationConfig, CollaborationRepository};

        let runtime_dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let repository = CollaborationRepository::new_with_startup_working_directory(
            runtime_dir.path(),
            CollaborationConfig::default(),
            workspace.path(),
        )
        .unwrap();
        let room_id = room_id_for("robot-1", "conversation-1");
        let snapshot = repository
            .ensure_room(&room_id, "钉钉 · 测试", &[])
            .unwrap();
        let recipients = vec![snapshot.room.default_member_id];
        let idempotency_key = "dingtalk:robot-1:message-1";

        let first = repository
            .post_message(
                &room_id,
                &recipients,
                "检查仓库",
                RoomInputMode::Chat,
                idempotency_key,
            )
            .unwrap();
        let repeated = repository
            .post_message(
                &room_id,
                &recipients,
                "检查仓库",
                RoomInputMode::Chat,
                idempotency_key,
            )
            .unwrap();

        assert!(!first.duplicate);
        assert!(repeated.duplicate);
        assert_eq!(first.event.event_id, repeated.event.event_id);
        assert_eq!(repository.snapshot(&room_id).unwrap().inbox.len(), 1);
    }

    #[test]
    fn session_webhook_rejects_non_dingtalk_or_credentialed_urls() {
        assert!(
            validate_session_webhook("https://api.dingtalk.com/agent/send?session=secret").is_ok()
        );
        assert!(validate_session_webhook("http://api.dingtalk.com/agent/send").is_err());
        assert!(validate_session_webhook("https://dingtalk.com.evil.test/send").is_err());
        assert!(validate_session_webhook("https://user:pass@api.dingtalk.com/send").is_err());
    }

    #[test]
    fn reply_chunks_preserve_unicode_and_empty_responses() {
        assert_eq!(split_text("甲乙丙丁戊", 2), vec!["甲乙", "丙丁", "戊"]);
        assert_eq!(split_text("", 2), vec!["（智脑返回了空响应）"]);
    }
}
