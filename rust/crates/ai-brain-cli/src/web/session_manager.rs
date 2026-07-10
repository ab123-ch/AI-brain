//! SessionManager — 内存 + 文件持久化的多会话管理
//!
//! 支持新建/切换/删除/列表操作，每个会话独立持久化为 JSON 文件。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::web::progress_adapter::ChatMessage;

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

    /// 按创建时间倒序列出所有会话
    pub fn list(&self) -> Vec<&WebSession> {
        let mut list: Vec<&WebSession> = self.sessions.values().collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }

    /// 向当前活跃会话追加一条消息并持久化
    pub fn push_message(&mut self, role: &str, content: &str) {
        let msg = ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            hidden: false,
        };

        let session = self.sessions.get_mut(&self.active_id).unwrap();
        session.messages.push(msg);

        // 若是第一条用户消息且标题是默认的，自动更新标题
        if session.title == "New Session" {
            if let Some(first_user) = session
                .messages
                .iter()
                .find(|m| m.role == "user" && !m.hidden)
            {
                let truncated: String = first_user.content.chars().take(30).collect();
                if first_user.content.chars().count() > 30 {
                    session.title = format!("{truncated}...");
                } else {
                    session.title = truncated;
                }
            }
        }

        // 持久化（clone 出 session 的关键数据，避免借用冲突）
        let id = self.active_id.clone();
        let dir = self.persist_dir.clone();
        if let Some(s) = self.sessions.get(&id) {
            Self::persist_to_disk(&dir, s);
        }
    }

    /// 向指定会话追加一条消息并持久化（不切换活跃会话）
    pub fn push_message_to(&mut self, session_id: &str, role: &str, content: &str) {
        let msg = ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            hidden: false,
        };

        if let Some(session) = self.sessions.get_mut(session_id) {
            session.messages.push(msg);

            // 若是第一条用户消息且标题是默认的，自动更新标题
            if session.title == "New Session" {
                if let Some(first_user) = session
                    .messages
                    .iter()
                    .find(|m| m.role == "user" && !m.hidden)
                {
                    let truncated: String = first_user.content.chars().take(30).collect();
                    if first_user.content.chars().count() > 30 {
                        session.title = format!("{truncated}...");
                    } else {
                        session.title = truncated;
                    }
                }
            }

            if let Some(s) = self.sessions.get(session_id) {
                Self::persist_to_disk(&self.persist_dir, s);
            }
        }
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
                        if let Ok(session) = serde_json::from_str::<WebSession>(&data) {
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
    fn active_mut_returns_mutable_ref() {
        let tmp = TempDir::new("test_session_active_mut");
        let mut mgr = SessionManager::new(tmp.path());
        mgr.active_mut().active_persona_id = "test-persona".to_string();
        assert_eq!(mgr.active().active_persona_id, "test-persona");
    }
}
