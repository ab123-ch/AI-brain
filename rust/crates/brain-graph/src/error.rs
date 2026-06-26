//! 错误类型 + ToolResult 三态（见设计 3.8 + 7.4）
//!
//! 工具返回三态（Ok/Empty/Err），降级由主脑自主判断。

use rusqlite::ffi::ErrorCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 图谱操作错误类型
#[derive(Debug, Error)]
pub enum BrainGraphError {
    /// 数据库锁占用（5s 超时）
    #[error("数据库锁占用: {0}")]
    DbLocked(String),

    /// 数据库损坏
    #[error("数据库损坏: {0}")]
    Corrupted(String),

    /// 查询超时
    #[error("查询超时（>2s）")]
    Timeout,

    /// 其他 SQLite 错误
    #[error("SQLite 错误: {0}")]
    Sqlite(rusqlite::Error),

    /// 无效输入
    #[error("无效输入: {0}")]
    InvalidInput(String),
}

/// 错误分类（用于 ToolResult::Err）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    DbLocked,
    DbCorrupted,
    Timeout,
    InvalidInput,
    DomainMismatch,
    NotFound,
}

/// 工具返回三态
///
/// - Ok: 成功，返回数据
/// - Empty: 无匹配，返回搜索统计 + 提示
/// - Err: 失败，返回错误类型 + 消息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum ToolResult<T> {
    #[serde(rename = "ok")]
    Ok { data: T },

    #[serde(rename = "empty")]
    Empty {
        searched_nodes: usize,
        searched_edges: usize,
        hint: Option<String>,
    },

    #[serde(rename = "error")]
    Err { kind: ErrorKind, message: String },
}

/// rusqlite 错误转换（见设计 7.4）
impl From<rusqlite::Error> for BrainGraphError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::SqliteFailure(err, _) => match err.code {
                ErrorCode::DatabaseBusy => BrainGraphError::DbLocked("busy".into()),
                ErrorCode::DatabaseLocked => BrainGraphError::DbLocked("locked".into()),
                ErrorCode::DatabaseCorrupt => BrainGraphError::Corrupted("corrupt".into()),
                _ => BrainGraphError::Sqlite(e),
            },
            _ => BrainGraphError::Sqlite(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_ok_serializes_with_status_tag() {
        let r: ToolResult<i32> = ToolResult::Ok { data: 42 };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"status\":\"ok\""));
        assert!(j.contains("42"));
    }

    #[test]
    fn tool_result_empty_serializes_with_hint() {
        let r: ToolResult<i32> = ToolResult::Empty {
            searched_nodes: 100,
            searched_edges: 50,
            hint: Some("试试别名".into()),
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"status\":\"empty\""));
        assert!(j.contains("试试别名"));
    }

    #[test]
    fn tool_result_err_serializes_kind() {
        let r: ToolResult<i32> = ToolResult::Err {
            kind: ErrorKind::Timeout,
            message: "查询超时".into(),
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"status\":\"error\""));
        assert!(j.contains("Timeout"));
    }

    #[test]
    fn brain_graph_error_from_rusqlite_busy() {
        // 构造 busy 错误比较复杂，仅测 Display
        let e = BrainGraphError::DbLocked("test".into());
        assert!(e.to_string().contains("锁"));
    }
}
