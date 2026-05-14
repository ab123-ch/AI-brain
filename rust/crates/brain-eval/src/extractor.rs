use brain_core::types::{ToolCallRecord, TurnRecord, TurnRole};

/// 单个文件变更记录
#[derive(Debug, Clone)]
pub struct FileChange {
    /// 文件路径
    pub file_path: String,
    /// 变更类型
    pub change_type: FileChangeType,
    /// edit_file 的 old_string（仅 Edit 有值）
    pub old_content: Option<String>,
    /// edit_file 的 new_string（仅 Edit 有值）
    pub new_content: Option<String>,
}

/// 变更类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeType {
    /// 编辑（edit_file）
    Edit,
    /// 新建/覆盖（write_file）
    Write,
}

/// 从 TurnRecord 列表中提取所有文件变更
pub fn extract_file_changes(turns: &[TurnRecord]) -> Vec<FileChange> {
    turns
        .iter()
        .filter(|t| {
            matches!(t.role, TurnRole::ToolCall)
                && t.tool_call.as_ref().map_or(false, |tc| !tc.is_error)
        })
        .filter_map(|t| {
            let tc = t.tool_call.as_ref()?;
            match tc.tool_name.as_str() {
                "edit_file" => extract_edit(tc),
                "write_file" => extract_write(tc),
                _ => None,
            }
        })
        .collect()
}

fn extract_edit(tc: &ToolCallRecord) -> Option<FileChange> {
    let path = tc.input.get("file_path")?.as_str()?.to_string();
    let old = tc
        .input
        .get("old_string")
        .and_then(|v| v.as_str())
        .map(String::from);
    let new = tc
        .input
        .get("new_string")
        .and_then(|v| v.as_str())
        .map(String::from);
    Some(FileChange {
        file_path: path,
        change_type: FileChangeType::Edit,
        old_content: old,
        new_content: new,
    })
}

fn extract_write(tc: &ToolCallRecord) -> Option<FileChange> {
    let path = tc.input.get("file_path")?.as_str()?.to_string();
    Some(FileChange {
        file_path: path,
        change_type: FileChangeType::Write,
        old_content: None,
        new_content: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::ToolCallRecord;
    use serde_json::json;

    fn make_edit_turn(path: &str, old: &str, new: &str, is_error: bool) -> TurnRecord {
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "edit_file".into(),
                input: json!({
                    "file_path": path,
                    "old_string": old,
                    "new_string": new,
                }),
                output: "OK".into(),
                duration_ms: 10,
                is_error,
            }),
            timestamp: String::new(),
        }
    }

    fn make_write_turn(path: &str, is_error: bool) -> TurnRecord {
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "write_file".into(),
                input: json!({
                    "file_path": path,
                    "content": "fn main() {}",
                }),
                output: "OK".into(),
                duration_ms: 10,
                is_error,
            }),
            timestamp: String::new(),
        }
    }

    fn make_grep_turn() -> TurnRecord {
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "grep_search".into(),
                input: json!({"pattern": "TODO"}),
                output: "3 matches".into(),
                duration_ms: 50,
                is_error: false,
            }),
            timestamp: String::new(),
        }
    }

    #[test]
    fn extract_edit_changes() {
        let turns = vec![
            make_edit_turn("src/a.rs", "fn old()", "fn new()", false),
            make_grep_turn(),
            make_edit_turn("src/b.rs", "old_val", "new_val", false),
        ];
        let changes = extract_file_changes(&turns);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].file_path, "src/a.rs");
        assert_eq!(changes[0].change_type, FileChangeType::Edit);
        assert_eq!(changes[0].old_content.as_deref(), Some("fn old()"));
        assert_eq!(changes[0].new_content.as_deref(), Some("fn new()"));
        assert_eq!(changes[1].file_path, "src/b.rs");
    }

    #[test]
    fn extract_write_changes() {
        let turns = vec![make_write_turn("src/new.rs", false)];
        let changes = extract_file_changes(&turns);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FileChangeType::Write);
        assert_eq!(changes[0].old_content, None);
    }

    #[test]
    fn skips_failed_tool_calls() {
        let turns = vec![
            make_edit_turn("src/a.rs", "old", "new", true),
            make_edit_turn("src/b.rs", "old", "new", false),
        ];
        let changes = extract_file_changes(&turns);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].file_path, "src/b.rs");
    }

    #[test]
    fn skips_non_file_tools() {
        let turns = vec![make_grep_turn()];
        let changes = extract_file_changes(&turns);
        assert!(changes.is_empty());
    }

    #[test]
    fn empty_turns() {
        let changes = extract_file_changes(&[]);
        assert!(changes.is_empty());
    }
}
