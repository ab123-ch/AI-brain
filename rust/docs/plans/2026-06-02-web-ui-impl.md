# 智脑 Web UI 实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 为智脑 AI 添加基于 Axum + WebSocket 的跨平台 Web 聊天界面。

**Architecture:** Axum HTTP server 内嵌前端静态文件（`include_str!`），WebSocket 双向通信复用 `ProgressEvent` 流式事件，`WebProgressEvent` 适配层处理不可序列化字段。多会话管理器维护独立对话历史，共享 Orchestrator 的记忆脑。

**Tech Stack:** Rust Axum 0.8 + tokio-tungstenite（Axum 内建 WebSocket） + 原生 HTML/JS/CSS + marked.js + highlight.js

**Design Doc:** `docs/plans/2026-06-02-web-ui-design.md`

---

## Task 1: WebProgressEvent 适配层

**目标：** 创建 `ProgressEvent` 的可序列化版本，去除不可序列化字段（`AskUser.response_tx`），供 WebSocket 传输。

**Files:**
- Create: `crates/ai-brain-cli/src/web/mod.rs`
- Create: `crates/ai-brain-cli/src/web/progress_adapter.rs`

**Step 1: 创建 web 模块入口**

```rust
// crates/ai-brain-cli/src/web/mod.rs
pub mod progress_adapter;
pub mod session_manager;
pub mod ws_handler;
```

**Step 2: 编写 WebProgressEvent 类型**

```rust
// crates/ai-brain-cli/src/web/progress_adapter.rs
use brain_core::types::ProgressEvent;
use serde::Serialize;

/// WebSocket 传输用的可序列化进度事件
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebProgressEvent {
    Connecting { brain: String, model: String },
    Thinking { brain: String },
    TextDelta { text: String },
    ThinkingDelta { content: String },
    ToolStart { brain: String, tool_name: String, input: String },
    ToolDone { brain: String, tool_name: String, duration_ms: u64, output_preview: String, is_error: bool },
    MemoryInjected { count: usize, preview: String },
    MemoryDetail { memories: Vec<String> },
    EvaluationStart,
    EvaluationResult { passed: bool, feedback: String },
    Evaluating,
    LlmRetry { attempt: u32, max_attempts: u32, error: String },
    AskUser { question: String, options: Option<Vec<String>>, multi_select: bool },
    Done,
    // 服务端控制消息（非 ProgressEvent）
    SessionList { sessions: Vec<SessionInfo> },
    SessionSwitched { session_id: String, messages: Vec<ChatMessage> },
    PersonaList { personas: Vec<PersonaInfo>, active_id: String },
    PersonaSwitched { persona_id: String, name: String },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub message_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonaInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

impl WebProgressEvent {
    /// 从 ProgressEvent 转换，AskUser 的 response_tx 被剥离
    pub fn from_progress(event: &ProgressEvent) -> Option<Self> {
        match event {
            ProgressEvent::Connecting { brain, model } => Some(Self::Connecting { brain: brain.clone(), model: model.clone() }),
            ProgressEvent::Thinking { brain } => Some(Self::Thinking { brain: brain.clone() }),
            ProgressEvent::TextDelta { text } => Some(Self::TextDelta { text: text.clone() }),
            ProgressEvent::ThinkingDelta { content } => Some(Self::ThinkingDelta { content: content.clone() }),
            ProgressEvent::ToolStart { brain, tool_name, input } => Some(Self::ToolStart { brain: brain.clone(), tool_name: tool_name.clone(), input: input.clone() }),
            ProgressEvent::ToolDone { brain, tool_name, duration_ms, output_preview, is_error } => Some(Self::ToolDone { brain: brain.clone(), tool_name: tool_name.clone(), duration_ms: *duration_ms, output_preview: output_preview.clone(), is_error: *is_error }),
            ProgressEvent::MemoryInjected { count, preview } => Some(Self::MemoryInjected { count: *count, preview: preview.clone() }),
            ProgressEvent::MemoryDetail { memories } => Some(Self::MemoryDetail { memories: memories.clone() }),
            ProgressEvent::EvaluationStart => Some(Self::EvaluationStart),
            ProgressEvent::EvaluationResult { passed, feedback } => Some(Self::EvaluationResult { passed: *passed, feedback: feedback.clone() }),
            ProgressEvent::Evaluating => Some(Self::Evaluating),
            ProgressEvent::LlmRetry { attempt, max_attempts, error } => Some(Self::LlmRetry { attempt: *attempt, max_attempts: *max_attempts, error: error.clone() }),
            ProgressEvent::AskUser { question, options, multi_select, .. } => Some(Self::AskUser { question: question.clone(), options: options.clone(), multi_select: *multi_select }),
            ProgressEvent::Done => Some(Self::Done),
            // EvaluationDetail 包含不可序列化复杂类型，跳过或简化
            ProgressEvent::EvaluationDetail { .. } => None,
        }
    }
}
```

**Step 3: 在 main.rs 注册模块**

在 `crates/ai-brain-cli/src/main.rs` 顶部添加:
```rust
mod web;
```

**Step 4: 编译验证**

Run: `cargo build -p ai-brain-cli 2>&1 | head -30`
Expected: 编译成功

**Step 5: 提交**

```bash
git add crates/ai-brain-cli/src/web/ crates/ai-brain-cli/src/main.rs
git commit -m "feat(web): 添加 WebProgressEvent 适配层"
```

---

## Task 2: SessionManager 多会话管理

**目标：** 实现内存 + 文件持久化的多会话管理，支持新建/切换/删除/列表操作。

**Files:**
- Create: `crates/ai-brain-cli/src/web/session_manager.rs`

**Step 1: 实现 SessionManager**

```rust
// crates/ai-brain-cli/src/web/session_manager.rs
use crate::web::progress_adapter::ChatMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSession {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub messages: Vec<ChatMessage>,
    pub active_persona_id: String,
}

pub struct SessionManager {
    sessions: HashMap<String, WebSession>,
    active_id: String,
    persist_dir: PathBuf,
}

impl SessionManager {
    pub fn new(base_dir: &Path) -> Self {
        let persist_dir = base_dir.join("web-sessions");
        std::fs::create_dir_all(&persist_dir).ok();
        let mut mgr = Self {
            sessions: HashMap::new(),
            active_id: String::new(),
            persist_dir,
        };
        mgr.load_all();
        if mgr.sessions.is_empty() {
            mgr.create_internal("新会话".into(), "default".into());
        }
        mgr
    }

    pub fn create(&mut self, title: String) -> &WebSession {
        let persona_id = self.active().active_persona_id.clone();
        self.create_internal(title, persona_id)
    }

    fn create_internal(&mut self, title: String, persona_id: String) -> &WebSession {
        let id = Uuid::new_v4().to_string()[..8].to_string();
        let session = WebSession {
            id: id.clone(),
            title,
            created_at: chrono::Utc::now().to_rfc3339(),
            messages: Vec::new(),
            active_persona_id: persona_id,
        };
        self.active_id = id.clone();
        self.sessions.insert(id.clone(), session);
        self.persist(&id);
        self.sessions.get(&id).unwrap()
    }

    pub fn switch(&mut self, id: &str) -> Option<&WebSession> {
        if self.sessions.contains_key(id) {
            self.active_id = id.to_string();
            Some(self.sessions.get(id).unwrap())
        } else {
            None
        }
    }

    pub fn delete(&mut self, id: &str) -> bool {
        if self.sessions.len() <= 1 { return false; }
        let removed = self.sessions.remove(id).is_some();
        if removed {
            let path = self.persist_dir.join(format!("{id}.json"));
            std::fs::remove_file(path).ok();
            if self.active_id == id {
                self.active_id = self.sessions.keys().next().unwrap().clone();
            }
        }
        removed
    }

    pub fn active(&self) -> &WebSession {
        self.sessions.get(&self.active_id).unwrap()
    }

    pub fn active_mut(&mut self) -> &mut WebSession {
        self.sessions.get_mut(&self.active_id).unwrap()
    }

    pub fn list(&self) -> Vec<&WebSession> {
        let mut s: Vec<_> = self.sessions.values().collect();
        s.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        s
    }

    pub fn push_message(&mut self, role: &str, content: &str) {
        let session = self.sessions.get_mut(&self.active_id).unwrap();
        session.messages.push(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        });
        // 自动生成标题（前 20 字符）
        if session.messages.len() == 1 && session.title == "新会话" {
            session.title = content.chars().take(20).collect();
        }
        self.persist(&self.active_id.clone());
    }

    fn persist(&self, id: &str) {
        if let Some(session) = self.sessions.get(id) {
            let path = self.persist_dir.join(format!("{id}.json"));
            if let Ok(data) = serde_json::to_string_pretty(session) {
                std::fs::write(path, data).ok();
            }
        }
    }

    fn load_all(&mut self) {
        if let Ok(entries) = std::fs::read_dir(&self.persist_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().map_or(false, |e| e == "json") {
                    if let Ok(data) = std::fs::read_to_string(entry.path()) {
                        if let Ok(session) = serde_json::from_str::<WebSession>(&data) {
                            self.sessions.insert(session.id.clone(), session);
                        }
                    }
                }
            }
        }
        if let Some(first) = self.sessions.keys().next() {
            self.active_id = first.clone();
        }
    }
}
```

**Step 2: 添加 uuid 依赖到 Cargo.toml**

在 `crates/ai-brain-cli/Cargo.toml` 的 `[dependencies]` 添加:
```toml
uuid = { version = "1", features = ["v4"] }
```

**Step 3: 编译验证**

Run: `cargo build -p ai-brain-cli 2>&1 | head -30`
Expected: 编译成功

**Step 4: 提交**

```bash
git add crates/ai-brain-cli/src/web/session_manager.rs crates/ai-brain-cli/Cargo.toml
git commit -m "feat(web): 添加 SessionManager 多会话管理"
```

---

## Task 3: WebSocket Handler 核心路由

**目标：** 实现 WebSocket 连接管理、消息解析和路由分发，连接 Orchestrator 的流式接口。

**Files:**
- Create: `crates/ai-brain-cli/src/web/ws_handler.rs`

**Step 1: 实现 WsHandler**

```rust
// crates/ai-brain-cli/src/web/ws_handler.rs
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::orchestrator::Orchestrator;
use crate::web::progress_adapter::WebProgressEvent;
use crate::web::session_manager::SessionManager;

/// 客户端发来的消息类型
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Query { input: String },
    Cancel,
    AskResponse { response: String },
    SwitchPersona { persona_id: String },
    NewSession,
    SwitchSession { session_id: String },
    DeleteSession { session_id: String },
}

/// 共享状态
pub struct AppState {
    pub orch: Arc<Orchestrator>,
    pub sessions: Arc<Mutex<SessionManager>>,
}

/// HTTP → WebSocket 升级入口
pub async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    // 发送初始状态
    let init_msgs = build_init_messages(&state).await;
    for msg in init_msgs {
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = socket.send(Message::Text(json.into())).await;
        }
    }

    // 消息循环
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(text) => {
                if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                    handle_client_message(&mut socket, &state, client_msg).await;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

async fn handle_client_message(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
    msg: ClientMessage,
) {
    match msg {
        ClientMessage::Query { input } => {
            // 记录用户消息
            {
                let mut sessions = state.sessions.lock().await;
                sessions.push_message("user", &input);
            }
            // 调用流式查询
            let (rx, handle, cancel) = Arc::clone(&state.orch).query_streaming(&input);
            let mut rx = rx;

            // 转发 ProgressEvent
            while let Some(event) = rx.recv().await {
                if let Some(web_event) = WebProgressEvent::from_progress(&event) {
                    if let Ok(json) = serde_json::to_string(&web_event) {
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            cancel.cancel();
                            break;
                        }
                    }
                }
            }

            // 获取最终结果
            if let Ok(Ok(output)) = handle.await {
                let mut sessions = state.sessions.lock().await;
                sessions.push_message("assistant", &output.answer);
            }
        }
        ClientMessage::Cancel => {
            // MVP: 暂不实现取消（需要保存 cancel token）
        }
        ClientMessage::AskResponse { response } => {
            // MVP: 暂不实现（需要保存 oneshot sender）
        }
        ClientMessage::SwitchPersona { persona_id } => {
            let mem = state.orch.memory_brain().lock().await;
            match mem.persona_manager_mut().switch(&persona_id) {
                Ok(persona) => {
                    let event = WebProgressEvent::PersonaSwitched {
                        persona_id: persona.id.clone(),
                        name: persona.name.clone(),
                    };
                    if let Ok(json) = serde_json::to_string(&event) {
                        let _ = socket.send(Message::Text(json.into())).await;
                    }
                }
                Err(e) => {
                    let event = WebProgressEvent::Error { message: e.to_string() };
                    if let Ok(json) = serde_json::to_string(&event) {
                        let _ = socket.send(Message::Text(json.into())).await;
                    }
                }
            }
        }
        ClientMessage::NewSession => {
            let mut sessions = state.sessions.lock().await;
            sessions.create("新会话".into());
            let info = build_session_list(&sessions);
            let active = sessions.active();
            let event = WebProgressEvent::SessionSwitched {
                session_id: active.id.clone(),
                messages: active.messages.clone(),
            };
            drop(sessions);

            if let Ok(json) = serde_json::to_string(&WebProgressEvent::SessionList { sessions: info }) {
                let _ = socket.send(Message::Text(json.into())).await;
            }
            if let Ok(json) = serde_json::to_string(&event) {
                let _ = socket.send(Message::Text(json.into())).await;
            }
        }
        ClientMessage::SwitchSession { session_id } => {
            let mut sessions = state.sessions.lock().await;
            if let Some(s) = sessions.switch(&session_id) {
                let event = WebProgressEvent::SessionSwitched {
                    session_id: s.id.clone(),
                    messages: s.messages.clone(),
                };
                drop(sessions);
                if let Ok(json) = serde_json::to_string(&event) {
                    let _ = socket.send(Message::Text(json.into())).await;
                }
            }
        }
        ClientMessage::DeleteSession { session_id } => {
            let mut sessions = state.sessions.lock().await;
            sessions.delete(&session_id);
            let info = build_session_list(&sessions);
            drop(sessions);
            if let Ok(json) = serde_json::to_string(&WebProgressEvent::SessionList { sessions: info }) {
                let _ = socket.send(Message::Text(json.into())).await;
            }
        }
    }
}

async fn build_init_messages(state: &Arc<AppState>) -> Vec<WebProgressEvent> {
    let sessions = state.sessions.lock().await;
    let session_list = build_session_list(&sessions);
    let active = sessions.active();
    let session_switched = WebProgressEvent::SessionSwitched {
        session_id: active.id.clone(),
        messages: active.messages.clone(),
    };
    drop(sessions);

    let mem = state.orch.memory_brain().lock().await;
    let personas: Vec<_> = mem.persona_manager().list().iter().map(|p| {
        crate::web::progress_adapter::PersonaInfo {
            id: p.id.clone(),
            name: p.name.clone(),
            description: p.description.clone(),
        }
    }).collect();
    let active_id = mem.persona_manager().active_id().to_string();
    drop(mem);

    vec![
        WebProgressEvent::SessionList { sessions: session_list },
        session_switched,
        WebProgressEvent::PersonaList { personas, active_id },
    ]
}

fn build_session_list(mgr: &SessionManager) -> Vec<crate::web::progress_adapter::SessionInfo> {
    mgr.list().iter().map(|s| crate::web::progress_adapter::SessionInfo {
        id: s.id.clone(),
        title: s.title.clone(),
        created_at: s.created_at.clone(),
        message_count: s.messages.len(),
    }).collect()
}
```

**Step 2: 添加 futures-util 依赖**

在 `crates/ai-brain-cli/Cargo.toml` 的 `[dependencies]` 添加:
```toml
futures-util = "0.3"
```

**Step 3: 编译验证**

Run: `cargo build -p ai-brain-cli 2>&1 | head -30`
Expected: 编译成功（可能有未使用 import 警告）

**Step 4: 提交**

```bash
git add crates/ai-brain-cli/src/web/ws_handler.rs crates/ai-brain-cli/Cargo.toml
git commit -m "feat(web): 添加 WebSocket Handler 核心路由"
```

---

## Task 4: 前端静态文件 + Axum 路由集成

**目标：** 创建内嵌式前端 HTML/JS/CSS 文件，集成到 Axum 路由中，新增 `--web` 启动模式。

**Files:**
- Create: `crates/ai-brain-cli/src/web/static/index.html`
- Create: `crates/ai-brain-cli/src/web/static/style.css`
- Create: `crates/ai-brain-cli/src/web/static/app.js`
- Modify: `crates/ai-brain-cli/src/api_server.rs` — 新增静态文件和 WebSocket 路由
- Modify: `crates/ai-brain-cli/src/main.rs` — 新增 `--web` 启动模式

**Step 1: 创建 index.html**

文件：`crates/ai-brain-cli/src/web/static/index.html`

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>智脑 AI</title>
    <link rel="stylesheet" href="/style.css">
</head>
<body>
    <!-- 顶栏 -->
    <header id="header">
        <div class="header-left">
            <button id="sidebar-toggle" class="icon-btn hidden-desktop">☰</button>
            <h1>🧠 智脑</h1>
        </div>
        <div class="header-center">
            <select id="persona-select"><option>加载中...</option></select>
        </div>
        <div class="header-right">
            <button id="thinking-toggle" class="icon-btn" title="切换思考内容">💭</button>
            <button id="new-session-btn" class="icon-btn" title="新建会话">＋</button>
        </div>
    </header>

    <!-- 主体 -->
    <div id="main">
        <!-- 侧边栏 -->
        <aside id="sidebar">
            <div id="session-list"></div>
        </aside>

        <!-- 聊天区 -->
        <main id="chat-area">
            <div id="messages"></div>
            <div id="input-area">
                <textarea id="input" placeholder="输入消息... (Enter 发送, Shift+Enter 换行)" rows="1"></textarea>
                <button id="send-btn">➤</button>
            </div>
        </main>
    </div>

    <!-- AskUser 弹窗 -->
    <div id="ask-modal" class="modal hidden">
        <div class="modal-content">
            <p id="ask-question"></p>
            <div id="ask-options"></div>
        </div>
    </div>

    <!-- 外部依赖 -->
    <script src="https://cdn.jsdelivr.net/npm/marked/marked.min.js"></script>
    <script src="https://cdn.jsdelivr.net/gh/highlightjs/cdn-release@11/build/highlight.min.js"></script>
    <script src="/app.js"></script>
</body>
</html>
```

**Step 2: 创建 style.css**

文件：`crates/ai-brain-cli/src/web/static/style.css`（深色主题，响应式布局，~200行）

```css
:root {
    --bg-primary: #1a1a2e;
    --bg-secondary: #16213e;
    --bg-chat: #0f3460;
    --bg-user-msg: #533483;
    --bg-ai-msg: #1a1a2e;
    --bg-tool: #1b2838;
    --text-primary: #e0e0e0;
    --text-secondary: #a0a0a0;
    --accent: #6c63ff;
    --success: #4caf50;
    --error: #f44336;
    --warning: #ff9800;
    --border: #2a2a4a;
    --radius: 12px;
    --sidebar-width: 240px;
}

* { margin: 0; padding: 0; box-sizing: border-box; }

body {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    background: var(--bg-primary);
    color: var(--text-primary);
    height: 100vh;
    display: flex;
    flex-direction: column;
    overflow: hidden;
}

/* 顶栏 */
#header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 16px;
    background: var(--bg-secondary);
    border-bottom: 1px solid var(--border);
    height: 48px;
    flex-shrink: 0;
}
#header h1 { font-size: 16px; font-weight: 600; }
.header-left, .header-right { display: flex; align-items: center; gap: 8px; }
.icon-btn {
    background: none; border: 1px solid var(--border); color: var(--text-primary);
    padding: 4px 10px; border-radius: 6px; cursor: pointer; font-size: 16px;
}
.icon-btn:hover { background: var(--border); }

#persona-select {
    background: var(--bg-primary); color: var(--text-primary);
    border: 1px solid var(--border); padding: 4px 8px; border-radius: 6px;
    font-size: 13px; max-width: 200px;
}

/* 主体 */
#main { display: flex; flex: 1; overflow: hidden; }

/* 侧边栏 */
#sidebar {
    width: var(--sidebar-width);
    background: var(--bg-secondary);
    border-right: 1px solid var(--border);
    overflow-y: auto;
    flex-shrink: 0;
    padding: 8px;
}
.session-item {
    padding: 8px 12px; border-radius: 8px; cursor: pointer;
    margin-bottom: 4px; font-size: 13px; color: var(--text-secondary);
    display: flex; justify-content: space-between; align-items: center;
}
.session-item:hover { background: var(--border); }
.session-item.active { background: var(--accent); color: white; }
.session-item .delete-btn {
    display: none; background: none; border: none; color: var(--error);
    cursor: pointer; font-size: 14px; padding: 0 4px;
}
.session-item:hover .delete-btn { display: block; }

/* 聊天区 */
#chat-area { flex: 1; display: flex; flex-direction: column; overflow: hidden; }
#messages {
    flex: 1; overflow-y: auto; padding: 16px;
    display: flex; flex-direction: column; gap: 12px;
}

/* 消息气泡 */
.msg { max-width: 80%; padding: 10px 14px; border-radius: var(--radius); line-height: 1.6; word-wrap: break-word; }
.msg.user { align-self: flex-end; background: var(--bg-user-msg); }
.msg.assistant { align-self: flex-start; background: var(--bg-ai-msg); border: 1px solid var(--border); }
.msg.system { align-self: center; color: var(--text-secondary); font-size: 12px; background: none; padding: 4px; max-width: 100%; }
.msg pre { background: #0d1117; padding: 12px; border-radius: 8px; overflow-x: auto; margin: 8px 0; }
.msg code { font-family: 'Fira Code', monospace; font-size: 13px; }

/* 工具调用 */
.tool-group { align-self: flex-start; max-width: 80%; }
.tool-item {
    background: var(--bg-tool); border: 1px solid var(--border);
    border-radius: 8px; margin-bottom: 4px; overflow: hidden; font-size: 13px;
}
.tool-header {
    padding: 6px 12px; cursor: pointer; display: flex;
    justify-content: space-between; align-items: center;
}
.tool-header .name { color: var(--accent); }
.tool-header .duration { color: var(--text-secondary); }
.tool-header .status-ok { color: var(--success); }
.tool-header .status-err { color: var(--error); }
.tool-body { display: none; padding: 8px 12px; border-top: 1px solid var(--border); max-height: 200px; overflow-y: auto; }
.tool-item.expanded .tool-body { display: block; }

/* 思考过程 */
.thinking-block {
    background: var(--bg-tool); border-left: 3px solid var(--warning);
    padding: 8px 12px; border-radius: 0 8px 8px 0; font-style: italic;
    color: var(--text-secondary); font-size: 13px; cursor: pointer;
    max-width: 80%; align-self: flex-start;
}
.thinking-block.collapsed { max-height: 40px; overflow: hidden; position: relative; }
.thinking-block.collapsed::after {
    content: '...'; position: absolute; bottom: 0; right: 0;
    background: var(--bg-tool); padding: 0 4px;
}

/* 流式状态 */
.spinner {
    align-self: flex-start; color: var(--accent); font-size: 14px;
    padding: 8px 12px;
}
.streaming-cursor::after { content: '▊'; animation: blink 1s infinite; }
@keyframes blink { 0%, 100% { opacity: 1; } 50% { opacity: 0; } }

/* 输入区 */
#input-area {
    display: flex; padding: 12px 16px; gap: 8px;
    border-top: 1px solid var(--border); background: var(--bg-secondary);
}
#input {
    flex: 1; background: var(--bg-primary); color: var(--text-primary);
    border: 1px solid var(--border); border-radius: 8px; padding: 10px 14px;
    font-size: 14px; resize: none; max-height: 120px; font-family: inherit;
    line-height: 1.5;
}
#input:focus { outline: none; border-color: var(--accent); }
#send-btn {
    background: var(--accent); color: white; border: none;
    border-radius: 8px; padding: 0 16px; cursor: pointer; font-size: 18px;
}
#send-btn:hover { opacity: 0.9; }
#send-btn:disabled { opacity: 0.5; cursor: not-allowed; }

/* 弹窗 */
.modal { position: fixed; inset: 0; background: rgba(0,0,0,0.5); display: flex; align-items: center; justify-content: center; z-index: 100; }
.modal.hidden { display: none; }
.modal-content { background: var(--bg-secondary); border-radius: 12px; padding: 24px; max-width: 400px; width: 90%; }
.modal-content p { margin-bottom: 16px; }
.ask-option {
    display: block; width: 100%; padding: 10px; margin-bottom: 8px;
    background: var(--bg-primary); border: 1px solid var(--border);
    border-radius: 8px; color: var(--text-primary); cursor: pointer; text-align: left;
}
.ask-option:hover { border-color: var(--accent); }

/* 响应式：移动端 */
@media (max-width: 768px) {
    .hidden-desktop { display: block !important; }
    #sidebar {
        position: fixed; left: -280px; top: 48px; bottom: 0;
        width: 280px; z-index: 50; transition: left 0.3s;
    }
    #sidebar.open { left: 0; }
    .msg { max-width: 95%; }
    .tool-group, .thinking-block { max-width: 95%; }
}
@media (min-width: 769px) {
    .hidden-desktop { display: none !important; }
}
```

**Step 3: 创建 app.js**

文件：`crates/ai-brain-cli/src/web/static/app.js`（~350行，WebSocket 客户端 + DOM 渲染）

```javascript
// 状态
let ws = null;
let thinkingVisible = false;
let isBusy = false;
let reconnectAttempts = 0;
const MAX_RECONNECT = 5;

// DOM 引用
const $messages = document.getElementById('messages');
const $input = document.getElementById('input');
const $sendBtn = document.getElementById('send-btn');
const $personaSelect = document.getElementById('persona-select');
const $sessionList = document.getElementById('session-list');
const $sidebar = document.getElementById('sidebar');
const $askModal = document.getElementById('ask-modal');
const $askQuestion = document.getElementById('ask-question');
const $askOptions = document.getElementById('ask-options');

// 连接 WebSocket
function connect() {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    ws = new WebSocket(`${proto}//${location.host}/ws`);

    ws.onopen = () => {
        reconnectAttempts = 0;
        console.log('WebSocket 已连接');
    };

    ws.onmessage = (e) => {
        const data = JSON.parse(e.data);
        handleServerMessage(data);
    };

    ws.onclose = () => {
        if (reconnectAttempts < MAX_RECONNECT) {
            reconnectAttempts++;
            const delay = Math.min(1000 * Math.pow(2, reconnectAttempts), 30000);
            setTimeout(connect, delay);
        }
    };

    ws.onerror = (e) => console.error('WebSocket 错误', e);
}

// 发送消息
function send(type, data = {}) {
    if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type, ...data }));
    }
}

// 处理服务端消息
function handleServerMessage(data) {
    switch (data.type) {
        case 'text_delta':
            appendStreamingText(data.text);
            break;
        case 'thinking_delta':
            appendThinking(data.content);
            break;
        case 'tool_start':
            addToolStart(data.tool_name, data.input);
            break;
        case 'tool_done':
            updateToolDone(data.tool_name, data.duration_ms, data.is_error, data.output_preview);
            break;
        case 'memory_injected':
            addSystemMessage(`💾 记忆注入: ${data.count} 条`);
            break;
        case 'evaluation_result':
            addSystemMessage(`${data.passed ? '✅' : '⚠️'} 评估: ${data.feedback}`);
            break;
        case 'connecting':
            setSpinner(`${data.brain}-连接中... (${data.model})`);
            break;
        case 'thinking':
            setSpinner(`${data.brain}-推理中...`);
            break;
        case 'evaluating':
            setSpinner('评估中...');
            break;
        case 'ask_user':
            showAskUser(data.question, data.options, data.multi_select);
            break;
        case 'done':
            finishStreaming();
            isBusy = false;
            $sendBtn.disabled = false;
            removeSpinner();
            break;
        case 'session_list':
            renderSessionList(data.sessions);
            break;
        case 'session_switched':
            renderMessages(data.messages);
            break;
        case 'persona_list':
            renderPersonaList(data.personas, data.active_id);
            break;
        case 'persona_switched':
            addSystemMessage(`🔄 已切换到: ${data.name}`);
            $personaSelect.value = data.persona_id;
            break;
        case 'error':
            addSystemMessage(`❌ ${data.message}`);
            isBusy = false;
            $sendBtn.disabled = false;
            break;
    }
}

// --- 流式文本渲染 ---
let currentStreamingEl = null;
let streamingText = '';

function appendStreamingText(text) {
    if (!currentStreamingEl) {
        currentStreamingEl = document.createElement('div');
        currentStreamingEl.className = 'msg assistant streaming-cursor';
        $messages.appendChild(currentStreamingEl);
        streamingText = '';
    }
    streamingText += text;
    currentStreamingEl.innerHTML = marked.parse(streamingText);
    currentStreamingEl.querySelectorAll('pre code').forEach(block => hljs.highlightElement(block));
    scrollToBottom();
}

function finishStreaming() {
    if (currentStreamingEl) {
        currentStreamingEl.classList.remove('streaming-cursor');
        currentStreamingEl = null;
        streamingText = '';
    }
}

// --- 思考过程 ---
let currentThinkingEl = null;
let thinkingText = '';

function appendThinking(content) {
    if (!currentThinkingEl) {
        currentThinkingEl = document.createElement('div');
        currentThinkingEl.className = 'thinking-block' + (thinkingVisible ? '' : ' collapsed');
        currentThinkingEl.onclick = () => currentThinkingEl.classList.toggle('collapsed');
        $messages.appendChild(currentThinkingEl);
        thinkingText = '';
    }
    thinkingText += content;
    currentThinkingEl.textContent = thinkingText;
    if (!thinkingVisible) currentThinkingEl.classList.add('collapsed');
    scrollToBottom();
}

// --- 工具调用 ---
let currentToolGroup = null;
const toolMap = new Map();

function addToolStart(name, input) {
    finishStreaming(); // 工具开始前先完成流式文本
    currentThinkingEl = null;

    if (!currentToolGroup) {
        currentToolGroup = document.createElement('div');
        currentToolGroup.className = 'tool-group';
        $messages.appendChild(currentToolGroup);
    }

    const id = name + '-' + Date.now();
    const el = document.createElement('div');
    el.className = 'tool-item';
    el.innerHTML = `
        <div class="tool-header" onclick="this.parentElement.classList.toggle('expanded')">
            <span class="name">⚙️ ${escapeHtml(name)}</span>
            <span class="duration">运行中...</span>
        </div>
        <div class="tool-body"><pre>${escapeHtml(truncate(input, 500))}</pre></div>
    `;
    currentToolGroup.appendChild(el);
    toolMap.set(id, { el, name });
    scrollToBottom();
}

function updateToolDone(name, durationMs, isError, preview) {
    // 查找最近的匹配工具
    if (currentToolGroup) {
        const items = currentToolGroup.querySelectorAll('.tool-item');
        for (let i = items.length - 1; i >= 0; i--) {
            const header = items[i].querySelector('.tool-header');
            if (header.textContent.includes(name)) {
                const durationEl = header.querySelector('.duration');
                const statusClass = isError ? 'status-err' : 'status-ok';
                durationEl.className = 'duration ' + statusClass;
                durationEl.textContent = durationMs + 'ms' + (isError ? ' ✗' : ' ✓');
                if (preview) {
                    const body = items[i].querySelector('.tool-body');
                    body.innerHTML = `<pre>${escapeHtml(truncate(preview, 500))}</pre>`;
                }
                break;
            }
        }
    }
    currentToolGroup = null;
}

// --- 系统消息 ---
function addSystemMessage(text) {
    const el = document.createElement('div');
    el.className = 'msg system';
    el.textContent = text;
    $messages.appendChild(el);
    scrollToBottom();
}

// --- Spinner ---
let spinnerEl = null;
function setSpinner(label) {
    if (!spinnerEl) {
        spinnerEl = document.createElement('div');
        spinnerEl.className = 'spinner';
        $messages.appendChild(spinnerEl);
    }
    spinnerEl.textContent = '⏳ ' + label;
    scrollToBottom();
}
function removeSpinner() {
    if (spinnerEl) { spinnerEl.remove(); spinnerEl = null; }
}

// --- 会话列表 ---
function renderSessionList(sessions) {
    $sessionList.innerHTML = sessions.map(s =>
        `<div class="session-item" data-id="${s.id}" onclick="switchSession('${s.id}')">
            <span>${escapeHtml(s.title)} (${s.message_count})</span>
            <button class="delete-btn" onclick="event.stopPropagation();deleteSession('${s.id}')">×</button>
        </div>`
    ).join('');
}

function renderMessages(messages) {
    $messages.innerHTML = '';
    messages.forEach(m => {
        const el = document.createElement('div');
        el.className = 'msg ' + m.role;
        if (m.role === 'assistant') {
            el.innerHTML = marked.parse(m.content);
            el.querySelectorAll('pre code').forEach(block => hljs.highlightElement(block));
        } else {
            el.textContent = m.content;
        }
        $messages.appendChild(el);
    });
    scrollToBottom();
}

// --- 人格列表 ---
function renderPersonaList(personas, activeId) {
    $personaSelect.innerHTML = personas.map(p =>
        `<option value="${p.id}" ${p.id === activeId ? 'selected' : ''}>${p.name}</option>`
    ).join('');
}

// --- AskUser 弹窗 ---
function showAskUser(question, options, multiSelect) {
    $askQuestion.textContent = question;
    $askOptions.innerHTML = '';
    if (options && options.length > 0) {
        options.forEach(opt => {
            const btn = document.createElement('button');
            btn.className = 'ask-option';
            btn.textContent = opt;
            btn.onclick = () => { send('ask_response', { response: opt }); $askModal.classList.add('hidden'); };
            $askOptions.appendChild(btn);
        });
    } else {
        const input = document.createElement('input');
        input.type = 'text'; input.style.cssText = 'width:100%;padding:10px;background:var(--bg-primary);color:var(--text-primary);border:1px solid var(--border);border-radius:8px;';
        const btn = document.createElement('button');
        btn.className = 'ask-option'; btn.textContent = '确认';
        btn.onclick = () => { send('ask_response', { response: input.value }); $askModal.classList.add('hidden'); };
        $askOptions.appendChild(input);
        $askOptions.appendChild(btn);
    }
    $askModal.classList.remove('hidden');
}

// --- 事件绑定 ---
$input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        submitQuery();
    }
});
$input.addEventListener('input', () => {
    $input.style.height = 'auto';
    $input.style.height = Math.min($input.scrollHeight, 120) + 'px';
});
$sendBtn.addEventListener('click', submitQuery);

$personaSelect.addEventListener('change', () => {
    send('switch_persona', { persona_id: $personaSelect.value });
});

document.getElementById('new-session-btn').addEventListener('click', () => {
    send('new_session');
});

document.getElementById('sidebar-toggle').addEventListener('click', () => {
    $sidebar.classList.toggle('open');
});

document.getElementById('thinking-toggle').addEventListener('click', () => {
    thinkingVisible = !thinkingVisible;
    document.querySelectorAll('.thinking-block').forEach(el => {
        el.classList.toggle('collapsed', !thinkingVisible);
    });
});

function submitQuery() {
    const text = $input.value.trim();
    if (!text || isBusy) return;
    isBusy = true;
    $sendBtn.disabled = true;

    // 显示用户消息
    const el = document.createElement('div');
    el.className = 'msg user';
    el.textContent = text;
    $messages.appendChild(el);

    $input.value = '';
    $input.style.height = 'auto';
    send('query', { input: text });
    scrollToBottom();
}

function switchSession(id) { send('switch_session', { session_id: id }); }
function deleteSession(id) { send('delete_session', { session_id: id }); }

// --- 工具函数 ---
function scrollToBottom() { $messages.scrollTop = $messages.scrollHeight; }
function escapeHtml(s) { const d = document.createElement('div'); d.textContent = s; return d.innerHTML; }
function truncate(s, n) { return s && s.length > n ? s.slice(0, n) + '...' : s || ''; }

// 启动
connect();
$input.focus();
```

**Step 4: 修改 api_server.rs 添加静态文件和 WebSocket 路由**

在现有 `api_server.rs` 文件中，**替换** `serve` 函数为：

```rust
// 在文件顶部添加:
use futures_util::SinkExt;
use tokio::sync::Mutex;

use crate::web::progress_adapter::{PersonaInfo, SessionInfo};
use crate::web::session_manager::SessionManager;
use crate::web::ws_handler::{ws_upgrade, AppState};

// 静态文件嵌入
static INDEX_HTML: &str = include_str!("web/static/index.html");
static APP_JS: &str = include_str!("web/static/app.js");
static STYLE_CSS: &str = include_str!("web/static/style.css");
```

修改 `serve` 函数:
```rust
pub async fn serve_web(orch: Orchestrator, addr: &str) {
    let base_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ai-brain");
    let sessions = Arc::new(Mutex::new(SessionManager::new(&base_dir)));
    let state = Arc::new(AppState {
        orch: Arc::new(orch),
        sessions,
    });

    let app = Router::new()
        // 静态文件
        .route("/", get(|| async { axum::http::StatusCode::FOUND }).fallback(|| async { INDEX_HTML }))
        .route("/style.css", get(|| async {
            ([(axum::http::header::CONTENT_TYPE, "text/css")], STYLE_CSS)
        }))
        .route("/app.js", get(|| async {
            ([(axum::http::header::CONTENT_TYPE, "application/javascript")], APP_JS)
        }))
        // WebSocket
        .route("/ws", get(ws_upgrade))
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("绑定 {addr} 失败: {e}");
            return;
        }
    };

    tracing::info!("Web UI 启动于 http://{addr}");
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("Web 服务错误: {e}");
    }
}
```

**Step 5: 在 main.rs 添加 --web 命令**

在 `Commands` 枚举中添加:
```rust
/// 启动 Web UI
Web {
    #[arg(long, default_value = "0.0.0.0:8080")]
    addr: String,
},
```

在 `run_command` 的 match 中添加:
```rust
Some(Commands::Web { addr }) => {
    let orch = init_or_die().await;
    api_server::serve_web(orch, addr).await;
}
```

**Step 6: 编译验证**

Run: `cargo build -p ai-brain-cli 2>&1 | head -40`
Expected: 编译成功

**Step 7: 提交**

```bash
git add crates/ai-brain-cli/src/
git commit -m "feat(web): 完成前端静态文件和 Axum 路由集成"
```

---

## Task 5: 编译修复与集成测试

**目标：** 确保整个 workspace 编译通过，修复所有编译错误，手动验证 Web UI 可访问。

**Files:**
- Modify: 各文件根据编译错误调整

**Step 1: 全量编译**

Run: `cargo build -p ai-brain-cli 2>&1`
Expected: 编译成功，如有错误逐一修复

**Step 2: 运行已有测试确保无回归**

Run: `cargo test --workspace --exclude brain-integration-tests 2>&1 | tail -20`
Expected: 所有已有测试通过

**Step 3: 手动启动 Web 服务**

Run: `cargo run -p ai-brain-cli -- web --addr 127.0.0.1:8080`
Expected: 终端显示 "Web UI 启动于 http://127.0.0.1:8080"

浏览器打开 `http://127.0.0.1:8080`，验证:
- 页面加载正常（深色主题）
- WebSocket 连接成功（浏览器控制台无错误）
- 会话列表显示（至少一个"新会话"）
- 人格下拉有数据
- 输入框可用，发送消息后能看到流式文本

**Step 4: 提交最终修复**

```bash
git add -A
git commit -m "fix(web): 编译修复与集成验证"
```

---

## Task 6: 提交整理与文档更新

**目标：** 清理代码，更新 MEMORY.md 记录。

**Files:**
- Modify: `/Users/chenh/.claude/projects/-Users-chenh-RustObject-claw-code-parity/memory/MEMORY.md`

**Step 1: 更新 MEMORY.md**

在"当前 Crate 结构"中添加:
```
├── ai-brain-cli/src/web/   # Web UI 模块
│   ├── mod.rs               # 模块入口
│   ├── ws_handler.rs        # WebSocket 连接管理
│   ├── session_manager.rs   # 多会话管理
│   ├── progress_adapter.rs  # ProgressEvent 适配
│   └── static/              # 内嵌前端文件
│       ├── index.html
│       ├── style.css
│       └── app.js
```

在开发进度中添加:
```
- **Web UI（2026-06-02）**: Axum + WebSocket + 嵌入式单页
  - 深色极简主题，响应式（桌面+移动端）
  - 流式交互：思考过程、文本流、工具调用、记忆注入、评估结果
  - 多会话管理：新建/切换/删除/文件持久化
  - 人格切换：下拉选择实时生效
  - 启动方式: `ai-brain-cli --web [--addr 0.0.0.0:8080]`
  - 设计文档: docs/plans/2026-06-02-web-ui-design.md
  - 实施计划: docs/plans/2026-06-02-web-ui-impl.md
```

**Step 2: 提交**

```bash
git add MEMORY.md
git commit -m "docs: 更新 MEMORY.md 记录 Web UI 模块"
```
