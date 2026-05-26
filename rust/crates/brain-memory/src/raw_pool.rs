//! L1 全量记忆基座（金字塔版）
//!
//! 基于 PyramidStorage 的 per-persona 原始记忆存储。
//! 路径: `personas/{persona_id}/pyramid/l1-raw/{session_id}.jsonl`

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::pyramid_storage::PyramidStorage;

/// L1 原始记忆条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawTurn {
    pub role: String,
    pub content: String,
    /// 可选的工具调用结果
    pub tool_output: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// L1 全量记忆基座
///
/// 职责:
/// - JSONL 持久化所有原始消息（永不删除）
/// - 按会话分文件存储
/// - 追加写入，不修改已有内容
/// - 路径 per-persona 隔离
pub struct RawPool {
    storage: PyramidStorage,
}

impl RawPool {
    /// 创建 RawPool
    pub fn new(storage: PyramidStorage) -> Self {
        Self { storage }
    }

    /// 追加一条 Turn 到 L1
    pub fn append_turn(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_output: Option<&str>,
    ) -> Result<()> {
        let turn = RawTurn {
            role: role.to_string(),
            content: content.to_string(),
            tool_output: tool_output.map(String::from),
            timestamp: Utc::now(),
        };
        let path = self.storage.l1_session_path(session_id);
        self.storage.append_jsonl(&path, &turn)
    }

    /// 读取一个会话的所有 Turn
    pub fn read_session(&self, session_id: &str) -> Result<Vec<RawTurn>> {
        let path = self.storage.l1_session_path(session_id);
        self.storage.read_jsonl(&path)
    }

    /// 列出所有会话 ID
    pub fn list_sessions(&self) -> Result<Vec<String>> {
        let files = self.storage.list_jsonl_files(&self.storage.l1_dir())?;
        Ok(files
            .iter()
            .filter_map(|f| f.file_stem().and_then(|s| s.to_str()).map(String::from))
            .collect())
    }

    /// 统计原始记忆条目数
    pub fn count(&self) -> Result<u32> {
        let sessions = self.list_sessions()?;
        let mut total = 0u32;
        for sid in &sessions {
            total += self.read_session(sid)?.len() as u32;
        }
        Ok(total)
    }

    /// 将整个会话序列化为 JSON（用于四步浓缩引擎的输入）
    pub fn session_to_json(&self, session_id: &str) -> Result<String> {
        let turns = self.read_session(session_id)?;
        Ok(serde_json::to_string_pretty(&turns)?)
    }

    /// 读取多个会话的完整内容（用于分析）
    pub fn read_sessions_batch(&self, session_ids: &[String]) -> Result<Vec<RawTurn>> {
        let mut all = Vec::new();
        for sid in session_ids {
            all.extend(self.read_session(sid)?);
        }
        Ok(all)
    }

    /// 获取底层 PyramidStorage 引用
    pub fn storage(&self) -> &PyramidStorage {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pool(persona_id: &str) -> (RawPool, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(tmp.path().to_path_buf(), persona_id);
        storage.ensure_dirs().unwrap();
        (RawPool::new(storage), tmp)
    }

    #[test]
    fn append_and_read_session() {
        let (pool, _tmp) = make_pool("test");
        pool.append_turn("sess-001", "User", "你好", None)
            .unwrap();
        pool.append_turn("sess-001", "Assistant", "你好！有什么可以帮你的？", None)
            .unwrap();

        let turns = pool.read_session("sess-001").unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "User");
        assert_eq!(turns[1].role, "Assistant");
    }

    #[test]
    fn append_with_tool_output() {
        let (pool, _tmp) = make_pool("test");
        pool.append_turn("sess-001", "Tool", "ls result", Some("file1.txt\nfile2.txt"))
            .unwrap();

        let turns = pool.read_session("sess-001").unwrap();
        assert_eq!(turns[0].tool_output.as_deref(), Some("file1.txt\nfile2.txt"));
    }

    #[test]
    fn list_multiple_sessions() {
        let (pool, _tmp) = make_pool("test");
        pool.append_turn("sess-a", "User", "hello", None)
            .unwrap();
        pool.append_turn("sess-b", "User", "world", None)
            .unwrap();
        pool.append_turn("sess-c", "User", "foo", None)
            .unwrap();

        let sessions = pool.list_sessions().unwrap();
        assert_eq!(sessions.len(), 3);
    }

    #[test]
    fn count_across_sessions() {
        let (pool, _tmp) = make_pool("test");
        pool.append_turn("sess-a", "User", "hello", None)
            .unwrap();
        pool.append_turn("sess-a", "Assistant", "hi", None)
            .unwrap();
        pool.append_turn("sess-b", "User", "world", None)
            .unwrap();

        assert_eq!(pool.count().unwrap(), 3);
    }

    #[test]
    fn read_nonexistent_session_returns_empty() {
        let (pool, _tmp) = make_pool("test");
        let turns = pool.read_session("nonexistent").unwrap();
        assert!(turns.is_empty());
    }

    #[test]
    fn persona_isolation() {
        let tmp = tempfile::tempdir().unwrap();

        // 创建两个不同人格的 RawPool
        let storage_a = PyramidStorage::new(tmp.path().to_path_buf(), "persona-a");
        storage_a.ensure_dirs().unwrap();
        let pool_a = RawPool::new(storage_a);

        let storage_b = PyramidStorage::new(tmp.path().to_path_buf(), "persona-b");
        storage_b.ensure_dirs().unwrap();
        let pool_b = RawPool::new(storage_b);

        // 写入不同数据
        pool_a
            .append_turn("sess-001", "User", "A的数据", None)
            .unwrap();
        pool_b
            .append_turn("sess-001", "User", "B的数据", None)
            .unwrap();

        // 验证隔离
        let turns_a = pool_a.read_session("sess-001").unwrap();
        let turns_b = pool_b.read_session("sess-001").unwrap();
        assert_eq!(turns_a[0].content, "A的数据");
        assert_eq!(turns_b[0].content, "B的数据");
    }

    #[test]
    fn session_to_json_outputs_valid_json() {
        let (pool, _tmp) = make_pool("test");
        pool.append_turn("sess-001", "User", "hello", None)
            .unwrap();

        let json = pool.session_to_json("sess-001").unwrap();
        let parsed: Vec<RawTurn> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn uses_persona_path() {
        let tmp = tempfile::tempdir().unwrap();
        let storage =
            PyramidStorage::new(tmp.path().to_path_buf(), "cyber-brain");
        storage.ensure_dirs().unwrap();
        let pool = RawPool::new(storage);

        pool.append_turn("sess-001", "User", "hello", None)
            .unwrap();

        let expected = tmp
            .path()
            .join("personas")
            .join("cyber-brain")
            .join("pyramid")
            .join("l1-raw")
            .join("sess-001.jsonl");
        assert!(expected.exists());
    }

    #[test]
    fn read_sessions_batch() {
        let (pool, _tmp) = make_pool("test");
        pool.append_turn("sess-a", "User", "a1", None).unwrap();
        pool.append_turn("sess-b", "User", "b1", None).unwrap();
        pool.append_turn("sess-b", "User", "b2", None).unwrap();

        let all = pool
            .read_sessions_batch(&["sess-a".into(), "sess-b".into()])
            .unwrap();
        assert_eq!(all.len(), 3);
    }
}
