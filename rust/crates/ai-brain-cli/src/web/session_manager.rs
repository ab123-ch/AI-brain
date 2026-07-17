//! SessionManager — 内存 + 文件持久化的多会话管理
//!
//! 支持新建/切换/删除/列表操作，每个会话独立持久化为 JSON 文件。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::runtime_trace::{ExchangePhase, RuntimeExchange};
use crate::web::progress_adapter::{ChatExchange, ChatMessage, ModifiedFileInfo};

// ─── WebSession ─────────────────────────────────────────────────────

/// 一个独立的聊天会话
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSession {
    /// 唯一 ID（UUID v4 前 8 位）
    pub id: String,
    /// 会话标题
    pub title: String,
    /// 创建时间（RFC3339）
    pub created_at: String,
    /// 对话历史
    pub messages: Vec<ChatMessage>,
    /// 当前人格 ID
    pub active_persona_id: String,
    #[serde(default)]
    pub modified_files: Vec<ModifiedFileInfo>,
}

// ─── SessionManager ─────────────────────────────────────────────────

/// 多会话管理器：内存 + JSON 文件持久化
pub struct SessionManager {
    /// 所有会话
    sessions: HashMap<String, WebSession>,
    /// 当前活跃会话 ID
    active_id: String,
    /// 持久化目录
    persist_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserQueryTurn {
    pub session_id: String,
    pub message_id: String,
    pub generation_id: String,
    pub input: String,
}

#[derive(Debug, Clone)]
pub struct ConversationFork {
    pub turn: UserQueryTurn,
    pub invalidated_generation_ids: Vec<String>,
    pub includes_legacy_unscoped: bool,
    pub messages: Vec<ChatMessage>,
}

impl SessionManager {
    /// 创建 SessionManager，从 `base_dir/web-sessions/` 加载所有会话。
    /// 若目录不存在或为空，则创建一个默认会话。
    pub fn new(base_dir: &std::path::Path) -> Self {
        let persist_dir = base_dir.join("web-sessions");

        // 确保目录存在
        if !persist_dir.exists() {
            let _ = fs::create_dir_all(&persist_dir);
        }

        let mut sessions = Self::load_all(&persist_dir);

        // 若没有会话，创建默认会话
        let active_id = if sessions.is_empty() {
            let default = Self::create_session("New Session".to_string());
            let id = default.id.clone();
            Self::persist_to_disk(&persist_dir, &default);
            sessions.insert(id.clone(), default);
            id
        } else {
            // 选择最新的会话
            sessions
                .values()
                .max_by(|a, b| a.created_at.cmp(&b.created_at))
                .map(|s| s.id.clone())
                .unwrap_or_else(|| {
                    let id = sessions.keys().next().unwrap().clone();
                    id
                })
        };

        Self {
            sessions,
            active_id,
            persist_dir,
        }
    }

    /// 新建一个会话，设为活跃并持久化。返回新会话的引用。
    pub fn create(&mut self, title: String) -> &WebSession {
        let session = Self::create_session(title);
        let id = session.id.clone();
        Self::persist_to_disk(&self.persist_dir, &session);
        self.sessions.insert(id.clone(), session);
        self.active_id = id;
        self.sessions.get(&self.active_id).unwrap()
    }

    /// 切换到指定 ID 的会话。返回 `Some(&WebSession)` 或 `None`。
    pub fn switch(&mut self, id: &str) -> Option<&WebSession> {
        if self.sessions.contains_key(id) {
            self.active_id = id.to_string();
            Some(self.sessions.get(&self.active_id).unwrap())
        } else {
            None
        }
    }

    /// 删除指定会话。至少保留一个会话，若只剩一个则返回 false。
    pub fn delete(&mut self, id: &str) -> bool {
        if self.sessions.len() <= 1 {
            return false;
        }
        if id == self.active_id {
            return false; // 不能删除当前活跃会话
        }
        if let Some(removed) = self.sessions.remove(id) {
            // 删除文件
            let path = Self::session_path(&self.persist_dir, &removed.id);
            let _ = fs::remove_file(path);
            true
        } else {
            false
        }
    }

    /// 获取当前活跃会话
    pub fn active(&self) -> &WebSession {
        self.sessions.get(&self.active_id).unwrap()
    }

    /// 获取当前活跃会话的可变引用
    pub fn active_mut(&mut self) -> &mut WebSession {
        self.sessions.get_mut(&self.active_id).unwrap()
    }

    /// Restore a previously cloned active session after a cross-component fork
    /// transaction fails before regeneration starts.
    pub fn restore_active_snapshot(&mut self, snapshot: WebSession) -> bool {
        if snapshot.id != self.active_id {
            return false;
        }
        Self::persist_to_disk(&self.persist_dir, &snapshot);
        self.sessions.insert(snapshot.id.clone(), snapshot);
        true
    }

    pub fn record_modified_file_to(
        &mut self,
        session_id: &str,
        path: &str,
    ) -> Vec<ModifiedFileInfo> {
        let normalized = std::path::PathBuf::from(path);
        let absolute = if normalized.is_absolute() {
            normalized
        } else {
            std::env::current_dir().unwrap_or_default().join(normalized)
        };
        let display_path = absolute
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();
        let name = absolute
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(path)
            .to_string();
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Vec::new();
        };
        session
            .modified_files
            .retain(|file| file.path != display_path);
        session.modified_files.insert(
            0,
            ModifiedFileInfo {
                name,
                path: display_path,
                updated_at: Utc::now(),
            },
        );
        session.modified_files.truncate(100);
        Self::persist_to_disk(&self.persist_dir, session);
        session.modified_files.clone()
    }

    /// 按创建时间倒序列出所有会话
    pub fn list(&self) -> Vec<&WebSession> {
        let mut list: Vec<&WebSession> = self.sessions.values().collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }

    /// 向当前活跃会话追加一条消息并持久化
    pub fn push_message(&mut self, role: &str, content: &str) {
        let msg = Self::create_chat_message(role, content);

        let session = self.sessions.get_mut(&self.active_id).unwrap();
        session.messages.push(msg);
        Self::refresh_default_title(session);

        // 持久化（clone 出 session 的关键数据，避免借用冲突）
        let id = self.active_id.clone();
        let dir = self.persist_dir.clone();
        if let Some(s) = self.sessions.get(&id) {
            Self::persist_to_disk(&dir, s);
        }
    }

    /// 向指定会话追加一条消息并持久化（不切换活跃会话）
    pub fn push_message_to(&mut self, session_id: &str, role: &str, content: &str) {
        let msg = Self::create_chat_message(role, content);

        if let Some(session) = self.sessions.get_mut(session_id) {
            session.messages.push(msg);
            Self::refresh_default_title(session);

            if let Some(s) = self.sessions.get(session_id) {
                Self::persist_to_disk(&self.persist_dir, s);
            }
        }
    }

    /// Append a new user turn and return the server-authoritative IDs used by
    /// MainBrain memory scoping.
    pub fn push_user_query(&mut self, content: &str) -> Result<UserQueryTurn, String> {
        let input = content.trim();
        if input.is_empty() {
            return Err("用户消息不能为空".into());
        }

        let message = Self::create_chat_message("user", input);
        let generation_id = message
            .memory_generation_id
            .clone()
            .expect("new user messages always have a memory generation");
        let turn = UserQueryTurn {
            session_id: self.active_id.clone(),
            message_id: message.id.clone(),
            generation_id,
            input: input.to_string(),
        };
        let session = self.sessions.get_mut(&self.active_id).unwrap();
        session.messages.push(message);
        Self::refresh_default_title(session);
        Self::persist_to_disk(&self.persist_dir, session);
        Ok(turn)
    }

    /// Edit a visible user message, replace its memory generation, and remove
    /// every later message from the authoritative conversation branch.
    pub fn edit_user_message(
        &mut self,
        message_id: &str,
        content: &str,
    ) -> Result<ConversationFork, String> {
        let input = content.trim();
        if input.is_empty() {
            return Err("用户消息不能为空".into());
        }
        self.fork_user_message(message_id, Some(input), false)
    }

    /// Retry only the final visible user turn. The existing message is reused;
    /// no duplicate user message is appended.
    pub fn retry_last_user_message(
        &mut self,
        message_id: &str,
    ) -> Result<ConversationFork, String> {
        self.fork_user_message(message_id, None, true)
    }

    /// Insert or update one paired runtime exchange in a specific chat session.
    pub fn upsert_exchange_to(
        &mut self,
        session_id: &str,
        generation_id: Option<&str>,
        exchange: RuntimeExchange,
    ) -> bool {
        let exchange_id = exchange.exchange_id.clone();
        let phase = exchange.phase;
        let timestamp = exchange.occurred_at;
        let title = exchange.title.clone();
        let Some(session) = self.sessions.get_mut(session_id) else {
            return false;
        };
        if generation_id.is_some_and(|generation_id| {
            !session.messages.iter().any(|message| {
                message.role == "user"
                    && message.memory_generation_id.as_deref() == Some(generation_id)
            })
        }) {
            // The originating user generation was superseded by edit/retry.
            return false;
        }

        if let Some(message) = session.messages.iter_mut().rev().find(|message| {
            message.role == "brain_communication"
                && message
                    .exchange
                    .as_ref()
                    .is_some_and(|stored| stored.exchange_id == exchange_id)
        }) {
            let stored = message.exchange.as_mut().expect("exchange record exists");
            match phase {
                ExchangePhase::Request => stored.request = Some(exchange),
                ExchangePhase::Response => stored.response = Some(exchange),
            }
        } else {
            let mut stored = ChatExchange {
                exchange_id,
                request: None,
                response: None,
            };
            match phase {
                ExchangePhase::Request => stored.request = Some(exchange),
                ExchangePhase::Response => stored.response = Some(exchange),
            }
            session.messages.push(ChatMessage {
                id: Self::new_message_id(),
                role: "brain_communication".into(),
                content: title,
                timestamp,
                hidden: false,
                exchange: Some(stored),
                memory_generation_id: generation_id.map(str::to_string),
            });
        }

        Self::persist_to_disk(&self.persist_dir, session);
        true
    }

    /// 返回当前活跃会话的可见消息。
    pub fn active_visible_messages(&self) -> Vec<ChatMessage> {
        self.active()
            .messages
            .iter()
            .filter(|m| !m.hidden)
            .cloned()
            .collect()
    }

    /// 隐藏当前活跃会话中某条可见消息所属的一整轮。
    ///
    /// 这里不会删除消息内容，只设置 hidden=true 并持久化。轮次按 user 消息切分：
    /// 从本条消息往前找到最近 user，再隐藏到下一个 user 之前的所有消息。
    pub fn hide_turn_by_visible_index(&mut self, visible_index: usize) -> Option<Vec<ChatMessage>> {
        let active_id = self.active_id.clone();
        let session = self.sessions.get_mut(&active_id)?;
        let raw_index = session
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.hidden)
            .nth(visible_index)
            .map(|(idx, _)| idx)?;

        let mut start = raw_index;
        while start > 0 && session.messages[start].role != "user" {
            start -= 1;
        }
        if session.messages[start].role != "user" {
            start = raw_index;
        }

        let mut end = start + 1;
        while end < session.messages.len() && session.messages[end].role != "user" {
            end += 1;
        }

        for msg in &mut session.messages[start..end] {
            msg.hidden = true;
        }

        let visible = session
            .messages
            .iter()
            .filter(|m| !m.hidden)
            .cloned()
            .collect::<Vec<_>>();
        Self::persist_to_disk(&self.persist_dir, session);
        Some(visible)
    }

    // ─── 私有辅助方法 ───────────────────────────────────────────────

    fn fork_user_message(
        &mut self,
        message_id: &str,
        edited_content: Option<&str>,
        require_last_user: bool,
    ) -> Result<ConversationFork, String> {
        let mut candidate = self.active().clone();
        let target_index = candidate
            .messages
            .iter()
            .position(|message| {
                !message.hidden && message.role == "user" && message.id == message_id
            })
            .ok_or_else(|| "用户消息不存在或已不在当前上下文中".to_string())?;

        if require_last_user {
            let last_user_index = candidate
                .messages
                .iter()
                .rposition(|message| !message.hidden && message.role == "user")
                .ok_or_else(|| "当前会话没有可重试的用户消息".to_string())?;
            if target_index != last_user_index {
                return Err("只能重试最后一条用户消息".into());
            }
        }

        let affected_users = candidate.messages[target_index..]
            .iter()
            .filter(|message| message.role == "user")
            .collect::<Vec<_>>();
        let invalidated_generation_ids = affected_users
            .iter()
            .filter_map(|message| message.memory_generation_id.clone())
            .collect::<Vec<_>>();
        let includes_legacy_unscoped = affected_users
            .iter()
            .any(|message| message.memory_generation_id.is_none());

        candidate.messages.truncate(target_index + 1);
        let target = candidate
            .messages
            .get_mut(target_index)
            .expect("target remains after truncation");
        if let Some(content) = edited_content {
            target.content = content.to_string();
        }
        let generation_id = Self::new_generation_id();
        target.memory_generation_id = Some(generation_id.clone());
        target.hidden = false;

        // Editing the first user turn should also update its session title.
        if candidate
            .messages
            .iter()
            .filter(|message| !message.hidden && message.role == "user")
            .next()
            .is_some_and(|message| message.id == message_id)
        {
            candidate.title = "New Session".into();
            Self::refresh_default_title(&mut candidate);
        }

        let turn = UserQueryTurn {
            session_id: candidate.id.clone(),
            message_id: message_id.to_string(),
            generation_id,
            input: candidate.messages[target_index].content.clone(),
        };
        let messages = candidate
            .messages
            .iter()
            .filter(|message| !message.hidden)
            .cloned()
            .collect::<Vec<_>>();
        Self::persist_to_disk(&self.persist_dir, &candidate);
        self.sessions.insert(candidate.id.clone(), candidate);

        Ok(ConversationFork {
            turn,
            invalidated_generation_ids,
            includes_legacy_unscoped,
            messages,
        })
    }

    fn create_chat_message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            id: Self::new_message_id(),
            role: role.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            hidden: false,
            exchange: None,
            memory_generation_id: (role == "user").then(Self::new_generation_id),
        }
    }

    fn new_message_id() -> String {
        format!("msg_{}", uuid::Uuid::new_v4().simple())
    }

    fn new_generation_id() -> String {
        format!("gen_{}", uuid::Uuid::new_v4().simple())
    }

    fn refresh_default_title(session: &mut WebSession) {
        if session.title != "New Session" {
            return;
        }
        let Some(first_user) = session
            .messages
            .iter()
            .find(|message| message.role == "user" && !message.hidden)
        else {
            return;
        };
        let truncated = first_user.content.chars().take(30).collect::<String>();
        session.title = if first_user.content.chars().count() > 30 {
            format!("{truncated}...")
        } else {
            truncated
        };
    }

    /// 创建一个新的 WebSession 实例（不插入到 map）
    fn create_session(title: String) -> WebSession {
        let id = uuid::Uuid::new_v4()
            .to_string()
            .split('-')
            .next()
            .unwrap_or("unknown")
            .to_string();
        WebSession {
            id,
            title,
            created_at: Utc::now().to_rfc3339(),
            messages: Vec::new(),
            active_persona_id: String::new(),
            modified_files: Vec::new(),
        }
    }

    /// 从磁盘加载所有会话文件
    fn load_all(dir: &std::path::Path) -> HashMap<String, WebSession> {
        let mut sessions = HashMap::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Ok(data) = fs::read_to_string(&path) {
                        if let Ok(mut session) = serde_json::from_str::<WebSession>(&data) {
                            let mut migrated = false;
                            for message in &mut session.messages {
                                if message.id.is_empty() {
                                    message.id = Self::new_message_id();
                                    migrated = true;
                                }
                            }
                            if migrated {
                                Self::persist_to_disk(dir, &session);
                            }
                            sessions.insert(session.id.clone(), session);
                        }
                    }
                }
            }
        }
        sessions
    }

    /// 将单个会话持久化到磁盘（静态方法，避免借用冲突）
    fn persist_to_disk(dir: &std::path::Path, session: &WebSession) {
        let path = Self::session_path(dir, &session.id);
        if let Ok(json) = serde_json::to_string_pretty(session) {
            let _ = fs::write(path, json);
        }
    }

    /// 构建会话文件路径
    fn session_path(dir: &std::path::Path, id: &str) -> PathBuf {
        dir.join(format!("{id}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 创建临时目录用于测试
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "{prefix}_{}_{}",
                std::process::id(),
                uuid::Uuid::new_v4().to_string().split('-').next().unwrap()
            ));
            let _ = fs::create_dir_all(&path);
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn new_creates_default_session() {
        let tmp = TempDir::new("test_session_new");
        let mgr = SessionManager::new(tmp.path());
        assert_eq!(mgr.sessions.len(), 1);
        assert_eq!(mgr.active().title, "New Session");
        // 验证文件已创建
        assert!(tmp.path().join("web-sessions").exists());
        let files: Vec<_> = fs::read_dir(tmp.path().join("web-sessions"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn create_adds_session() {
        let tmp = TempDir::new("test_session_create");
        let mut mgr = SessionManager::new(tmp.path());
        let s = mgr.create("Test Session".to_string());
        assert_eq!(s.title, "Test Session");
        assert_eq!(mgr.sessions.len(), 2);
        assert_eq!(mgr.active().title, "Test Session");
    }

    #[test]
    fn switch_changes_active() {
        let tmp = TempDir::new("test_session_switch");
        let mut mgr = SessionManager::new(tmp.path());
        let s1_id = mgr.active().id.clone();
        let s2 = mgr.create("Second".to_string());
        let s2_id = s2.id.clone();

        // 切换回第一个
        let switched = mgr.switch(&s1_id);
        assert!(switched.is_some());
        assert_eq!(mgr.active().id, s1_id);

        // 切换到第二个
        let switched = mgr.switch(&s2_id);
        assert!(switched.is_some());
        assert_eq!(mgr.active().id, s2_id);

        // 不存在的 ID
        let switched = mgr.switch("nonexistent");
        assert!(switched.is_none());
    }

    #[test]
    fn delete_removes_session() {
        let tmp = TempDir::new("test_session_delete");
        let mut mgr = SessionManager::new(tmp.path());
        let s2 = mgr.create("Second".to_string());
        let s2_id = s2.id.clone();

        // 切换到第二个，再切回默认，然后删除第二个
        let default_id = {
            let list = mgr.list();
            let other = list.iter().find(|s| s.id != s2_id).unwrap();
            other.id.clone()
        };
        mgr.switch(&default_id);
        assert!(mgr.delete(&s2_id));
        assert_eq!(mgr.sessions.len(), 1);
    }

    #[test]
    fn delete_cannot_remove_last() {
        let tmp = TempDir::new("test_session_delete_last");
        let mut mgr = SessionManager::new(tmp.path());
        let id = mgr.active().id.clone();
        assert!(!mgr.delete(&id)); // 只有一个会话
        assert_eq!(mgr.sessions.len(), 1);
    }

    #[test]
    fn delete_cannot_remove_active() {
        let tmp = TempDir::new("test_session_delete_active");
        let mut mgr = SessionManager::new(tmp.path());
        mgr.create("Second".to_string());
        // active 是第二个
        let active_id = mgr.active().id.clone();
        assert!(!mgr.delete(&active_id)); // 不能删除活跃会话
        assert_eq!(mgr.sessions.len(), 2);
    }

    #[test]
    fn list_sorted_by_created_at_desc() {
        let tmp = TempDir::new("test_session_list");
        let mut mgr = SessionManager::new(tmp.path());
        mgr.create("Session B".to_string());
        mgr.create("Session C".to_string());

        let list = mgr.list();
        assert!(list.len() >= 3);
        // 最新的在最前面
        assert_eq!(list[0].title, "Session C");
    }

    #[test]
    fn push_message_appends_and_persists() {
        let tmp = TempDir::new("test_session_push");
        let mut mgr = SessionManager::new(tmp.path());

        mgr.push_message("user", "Hello");
        assert_eq!(mgr.active().messages.len(), 1);
        assert_eq!(mgr.active().messages[0].role, "user");
        assert_eq!(mgr.active().messages[0].content, "Hello");

        mgr.push_message("assistant", "Hi there!");
        assert_eq!(mgr.active().messages.len(), 2);

        // 标题应自动更新为第一条用户消息内容
        assert!(mgr.active().title.contains("Hello"));
    }

    #[test]
    fn hide_turn_hides_whole_visible_turn_but_keeps_messages() {
        let tmp = TempDir::new("test_session_hide_turn");
        let mut mgr = SessionManager::new(tmp.path());

        mgr.push_message("user", "第一轮");
        mgr.push_message("assistant", "第一轮回复");
        mgr.push_message("user", "第二轮");
        mgr.push_message("assistant", "第二轮回复");

        let visible = mgr.hide_turn_by_visible_index(1).unwrap();
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].content, "第二轮");
        assert_eq!(visible[1].content, "第二轮回复");
        assert_eq!(mgr.active().messages.len(), 4);
        assert!(mgr.active().messages[0].hidden);
        assert!(mgr.active().messages[1].hidden);
        assert!(!mgr.active().messages[2].hidden);
    }

    #[test]
    fn push_message_title_truncation() {
        let tmp = TempDir::new("test_session_truncate");
        let mut mgr = SessionManager::new(tmp.path());

        let long_msg = "This is a very long message that should be truncated to thirty characters";
        mgr.push_message("user", long_msg);
        assert!(mgr.active().title.len() <= 33); // 30 chars + "..."
        assert!(mgr.active().title.ends_with('.'));
    }

    #[test]
    fn persistence_roundtrip() {
        let tmp = TempDir::new("test_session_roundtrip");

        // 创建并写入消息
        let id = {
            let mut mgr = SessionManager::new(tmp.path());
            mgr.push_message("user", "persist test");
            let id = mgr.active().id.clone();
            id
        };

        // 重新加载
        let mgr2 = SessionManager::new(tmp.path());
        assert_eq!(mgr2.sessions.len(), 1);
        assert_eq!(mgr2.active().id, id);
        assert_eq!(mgr2.active().messages.len(), 1);
        assert_eq!(mgr2.active().messages[0].content, "persist test");
    }

    #[test]
    fn exchange_request_and_response_pair_and_persist() {
        use crate::runtime_trace::{ExchangeKind, ExchangeStatus};

        let tmp = TempDir::new("test_session_exchange");
        let session_id = {
            let mut mgr = SessionManager::new(tmp.path());
            let session_id = mgr.active().id.clone();
            let request = RuntimeExchange::new(
                "delegation-1",
                "main",
                "主脑",
                "novel:1",
                "小说脑",
                ExchangeKind::Delegation,
                ExchangePhase::Request,
                "设计章节",
                "完整任务原文",
                ExchangeStatus::Running,
                None,
            );
            let response = RuntimeExchange::new(
                "delegation-1",
                "novel:1",
                "小说脑",
                "main",
                "主脑",
                ExchangeKind::Delegation,
                ExchangePhase::Response,
                "设计章节 · 最终结果",
                "完整小说结果",
                ExchangeStatus::Completed,
                Some(42),
            );

            assert!(mgr.upsert_exchange_to(&session_id, None, request));
            assert!(mgr.upsert_exchange_to(&session_id, None, response));
            assert_eq!(mgr.active().messages.len(), 1);
            let stored = mgr.active().messages[0].exchange.as_ref().unwrap();
            assert_eq!(stored.request.as_ref().unwrap().content, "完整任务原文");
            assert_eq!(stored.response.as_ref().unwrap().content, "完整小说结果");
            session_id
        };

        let reloaded = SessionManager::new(tmp.path());
        assert_eq!(reloaded.active().id, session_id);
        let stored = reloaded.active().messages[0].exchange.as_ref().unwrap();
        assert_eq!(stored.exchange_id, "delegation-1");
        assert_eq!(stored.response.as_ref().unwrap().content, "完整小说结果");
    }

    #[test]
    fn hiding_turn_also_hides_its_brain_exchange() {
        use crate::runtime_trace::{ExchangeKind, ExchangeStatus};

        let tmp = TempDir::new("test_session_hide_exchange");
        let mut mgr = SessionManager::new(tmp.path());
        let session_id = mgr.active().id.clone();
        mgr.push_message("user", "请小说脑设计章节");
        assert!(mgr.upsert_exchange_to(
            &session_id,
            None,
            RuntimeExchange::new(
                "delegation-hide",
                "main",
                "主脑",
                "novel:1",
                "小说脑",
                ExchangeKind::Delegation,
                ExchangePhase::Request,
                "设计章节",
                "完整任务原文",
                ExchangeStatus::Running,
                None,
            ),
        ));
        mgr.push_message("assistant", "小说脑正在处理");
        mgr.push_message("user", "下一轮");
        mgr.push_message("assistant", "下一轮回复");

        let visible = mgr.hide_turn_by_visible_index(1).unwrap();

        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].content, "下一轮");
        assert_eq!(visible[1].content, "下一轮回复");
        assert!(mgr.active().messages[..3]
            .iter()
            .all(|message| message.hidden));
    }

    #[test]
    fn active_mut_returns_mutable_ref() {
        let tmp = TempDir::new("test_session_active_mut");
        let mut mgr = SessionManager::new(tmp.path());
        mgr.active_mut().active_persona_id = "test-persona".to_string();
        assert_eq!(mgr.active().active_persona_id, "test-persona");
    }

    #[test]
    fn editing_user_message_forks_history_and_replaces_memory_generation() {
        let tmp = TempDir::new("test_session_edit_fork");
        let mut mgr = SessionManager::new(tmp.path());
        let first = mgr.push_user_query("第一条原文").unwrap();
        mgr.push_message("assistant", "第一条回复");
        let second = mgr.push_user_query("第二条原文").unwrap();
        mgr.push_message("assistant", "第二条回复");

        let fork = mgr
            .edit_user_message(&first.message_id, "第一条修订")
            .unwrap();

        assert_eq!(fork.turn.message_id, first.message_id);
        assert_eq!(fork.turn.input, "第一条修订");
        assert_ne!(fork.turn.generation_id, first.generation_id);
        assert_eq!(
            fork.invalidated_generation_ids,
            vec![first.generation_id, second.generation_id]
        );
        assert!(!fork.includes_legacy_unscoped);
        assert_eq!(fork.messages.len(), 1);
        assert_eq!(mgr.active().messages.len(), 1);
        assert_eq!(mgr.active().messages[0].content, "第一条修订");

        let reloaded = SessionManager::new(tmp.path());
        assert_eq!(reloaded.active().messages.len(), 1);
        assert_eq!(
            reloaded.active().messages[0].memory_generation_id,
            Some(fork.turn.generation_id)
        );
    }

    #[test]
    fn retry_reuses_only_the_last_user_message_without_duplication() {
        let tmp = TempDir::new("test_session_retry_last");
        let mut mgr = SessionManager::new(tmp.path());
        let first = mgr.push_user_query("第一条").unwrap();
        mgr.push_message("assistant", "第一条回复");
        let second = mgr.push_user_query("第二条").unwrap();
        mgr.push_message("assistant", "第二条回复");

        let error = mgr.retry_last_user_message(&first.message_id).unwrap_err();
        assert_eq!(error, "只能重试最后一条用户消息");

        let fork = mgr.retry_last_user_message(&second.message_id).unwrap();
        assert_eq!(fork.turn.message_id, second.message_id);
        assert_eq!(fork.turn.input, "第二条");
        assert_ne!(fork.turn.generation_id, second.generation_id);
        assert_eq!(fork.invalidated_generation_ids, vec![second.generation_id]);
        assert_eq!(mgr.active().messages.len(), 3);
        assert_eq!(
            mgr.active()
                .messages
                .iter()
                .filter(|message| message.role == "user")
                .count(),
            2
        );
        assert_eq!(mgr.active().messages.last().unwrap().id, second.message_id);
    }

    #[test]
    fn editing_legacy_user_message_marks_unscoped_memory() {
        let tmp = TempDir::new("test_session_edit_legacy");
        let mut mgr = SessionManager::new(tmp.path());
        let turn = mgr.push_user_query("旧消息").unwrap();
        mgr.active_mut().messages[0].memory_generation_id = None;
        mgr.push_message("assistant", "旧回复");

        let fork = mgr.edit_user_message(&turn.message_id, "新消息").unwrap();
        assert!(fork.includes_legacy_unscoped);
        assert!(fork.invalidated_generation_ids.is_empty());
    }

    #[test]
    fn late_exchange_from_superseded_generation_is_rejected() {
        use crate::runtime_trace::{ExchangeKind, ExchangeStatus};

        let tmp = TempDir::new("test_session_stale_exchange");
        let mut mgr = SessionManager::new(tmp.path());
        let original = mgr.push_user_query("需要后台代理").unwrap();
        mgr.push_message("assistant", "已提交后台任务");
        let fork = mgr.retry_last_user_message(&original.message_id).unwrap();
        let stale_response = RuntimeExchange::new(
            "delegation-late",
            "agent:1",
            "后台代理",
            "main",
            "主脑",
            ExchangeKind::Delegation,
            ExchangePhase::Response,
            "迟到结果",
            "旧分支结果",
            ExchangeStatus::Completed,
            Some(10),
        );

        assert!(!mgr.upsert_exchange_to(
            &original.session_id,
            Some(&original.generation_id),
            stale_response.clone(),
        ));
        assert_eq!(mgr.active().messages.len(), 1);
        assert!(mgr.upsert_exchange_to(
            &original.session_id,
            Some(&fork.turn.generation_id),
            stale_response,
        ));
        assert_eq!(mgr.active().messages.len(), 2);
    }
}
