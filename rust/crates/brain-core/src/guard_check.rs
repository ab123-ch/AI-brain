//! Safety guard for tool execution.
//!
//! Before executing a tool call from the LLM, run `guard_check()` to decide
//! whether user confirmation is required. This prevents the agent from
//! silently performing destructive operations.

use crate::types::ToolCall;

// ---------------------------------------------------------------------------
// Guard Result
// ---------------------------------------------------------------------------

/// Result of a guard check on a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardResult {
    /// Tool execution is safe to proceed.
    Pass,
    /// User confirmation is required before execution.
    NeedUserConfirm {
        /// Human-readable reason for the confirmation request.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Destructive Bash Patterns
// ---------------------------------------------------------------------------

/// Bash command patterns that are considered destructive.
const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "rm -rf",
    "rm -r",
    "rmdir",
    "mkfs",
    "dd if=",
    "format",
    "del /",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "git push --force",
    "git reset --hard",
    "git clean",
    "git checkout --",
    "drop table",
    "drop database",
    "truncate",
];

/// File paths that are considered sensitive (writes require confirmation).
const SENSITIVE_PATHS: &[&str] = &[
    "/etc/",
    "/usr/",
    "/bin/",
    "/sbin/",
    "/System/",
    "/Library/",
    ".env",
    "credentials",
    "id_rsa",
    "id_ed25519",
    ".ssh/",
    ".gnupg/",
    ".gitconfig",
];

// ---------------------------------------------------------------------------
// Guard Check Function
// ---------------------------------------------------------------------------

/// Check whether a tool call is safe to execute without user confirmation.
///
/// # Rules
///
/// - `bash` with destructive patterns → NeedUserConfirm
/// - `write_file` / `edit_file` targeting sensitive paths → NeedUserConfirm
/// - Everything else → Pass
pub fn guard_check(tool_call: &ToolCall) -> GuardResult {
    match tool_call.tool_name.as_str() {
        "bash" => check_bash_command(tool_call),
        "write_file" | "edit_file" => check_file_path(tool_call),
        _ => GuardResult::Pass,
    }
}

/// Check if a bash command contains destructive patterns.
fn check_bash_command(tool_call: &ToolCall) -> GuardResult {
    let command = tool_call
        .input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let command_lower = command.to_lowercase();

    for pattern in DESTRUCTIVE_PATTERNS {
        if command_lower.contains(pattern) {
            return GuardResult::NeedUserConfirm {
                reason: format!(
                    "Bash command contains potentially destructive pattern: \"{pattern}\""
                ),
            };
        }
    }

    GuardResult::Pass
}

/// Check if a file write/edit targets a sensitive path.
fn check_file_path(tool_call: &ToolCall) -> GuardResult {
    let path = tool_call
        .input
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    for sensitive in SENSITIVE_PATHS {
        if path.contains(sensitive) {
            return GuardResult::NeedUserConfirm {
                reason: format!(
                    "File operation targets a sensitive path containing \"{sensitive}\""
                ),
            };
        }
    }

    GuardResult::Pass
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash_call(cmd: &str) -> ToolCall {
        ToolCall {
            tool_name: "bash".into(),
            input: json!({ "command": cmd }),
            validated: false,
            validation_id: None,
        }
    }

    fn write_call(path: &str) -> ToolCall {
        ToolCall {
            tool_name: "write_file".into(),
            input: json!({ "path": path, "content": "hello" }),
            validated: false,
            validation_id: None,
        }
    }

    #[test]
    fn safe_bash_passes() {
        assert_eq!(guard_check(&bash_call("ls -la")), GuardResult::Pass);
        assert_eq!(guard_check(&bash_call("cargo test")), GuardResult::Pass);
        assert_eq!(guard_check(&bash_call("git status")), GuardResult::Pass);
    }

    #[test]
    fn destructive_bash_needs_confirm() {
        assert!(matches!(
            guard_check(&bash_call("rm -rf /tmp/test")),
            GuardResult::NeedUserConfirm { .. }
        ));
        assert!(matches!(
            guard_check(&bash_call("git push --force")),
            GuardResult::NeedUserConfirm { .. }
        ));
        assert!(matches!(
            guard_check(&bash_call("git reset --hard HEAD~1")),
            GuardResult::NeedUserConfirm { .. }
        ));
    }

    #[test]
    fn safe_write_passes() {
        assert_eq!(guard_check(&write_call("/tmp/test.rs")), GuardResult::Pass);
        assert_eq!(guard_check(&write_call("src/main.rs")), GuardResult::Pass);
    }

    #[test]
    fn sensitive_path_needs_confirm() {
        assert!(matches!(
            guard_check(&write_call("/etc/hosts")),
            GuardResult::NeedUserConfirm { .. }
        ));
        assert!(matches!(
            guard_check(&write_call("config/.env")),
            GuardResult::NeedUserConfirm { .. }
        ));
        assert!(matches!(
            guard_check(&write_call("/home/user/.ssh/config")),
            GuardResult::NeedUserConfirm { .. }
        ));
    }

    #[test]
    fn read_file_always_passes() {
        let call = ToolCall {
            tool_name: "read_file".into(),
            input: json!({ "path": "/etc/shadow" }),
            validated: false,
            validation_id: None,
        };
        assert_eq!(guard_check(&call), GuardResult::Pass);
    }

    #[test]
    fn unknown_tool_passes() {
        let call = ToolCall {
            tool_name: "custom_tool".into(),
            input: json!({}),
            validated: false,
            validation_id: None,
        };
        assert_eq!(guard_check(&call), GuardResult::Pass);
    }
}
