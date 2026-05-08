//! 延迟分析：关闭时快速保存对话到磁盘，启动时注入+后台分析
//!
//! 替代关闭时同步执行四步分析（30-120秒），改为：
//! - 关闭时：只保存 JSON（毫秒级）
//! - 启动时：注入原始对话到主脑 + 后台跑四步分析

use serde::{Deserialize, Serialize};
use std::path::Path;

const PENDING_FILE: &str = "pending_analysis.json";

/// 未分析的对话数据
#[derive(Debug, Serialize, Deserialize)]
pub struct PendingAnalysis {
    /// 原始对话文本列表
    pub conversations: Vec<String>,
    /// 会话 ID
    pub session_id: String,
    /// 保存时间
    pub saved_at: String,
}

impl PendingAnalysis {
    /// 保存到磁盘（毫秒级）
    pub fn save(
        base_dir: &Path,
        conversations: Vec<String>,
        session_id: String,
    ) -> std::io::Result<()> {
        if conversations.is_empty() {
            return Ok(());
        }
        let pending = Self {
            conversations,
            session_id,
            saved_at: chrono::Utc::now().to_rfc3339(),
        };
        let path = base_dir.join(PENDING_FILE);
        let json = serde_json::to_string_pretty(&pending)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// 从磁盘读取并删除（一次性消费）
    pub fn load(base_dir: &Path) -> Option<Self> {
        let path = base_dir.join(PENDING_FILE);
        if !path.exists() {
            return None;
        }
        let data = std::fs::read_to_string(&path).ok()?;
        let pending: Self = serde_json::from_str(&data).ok()?;
        // 读取后删除，防止重复注入
        let _ = std::fs::remove_file(&path);
        Some(pending)
    }

    /// 格式化为可读文本，供主脑注入
    pub fn format_for_injection(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "[上次会话记忆（会话 {}，保存于 {}）— 以下是你上次和用户的对话摘要]",
            self.session_id, self.saved_at
        ));
        lines.push(String::new());

        for (i, conv) in self.conversations.iter().enumerate() {
            // 每条对话限制 500 字，防止注入过长
            let truncated = if conv.chars().count() > 500 {
                let s: String = conv.chars().take(500).collect();
                format!("{s}...")
            } else {
                conv.clone()
            };
            lines.push(format!("{}. {truncated}", i + 1));
        }

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let convs = vec!["用户：你好".to_string(), "助手：你好！".to_string()];
        PendingAnalysis::save(base, convs.clone(), "sess-test-123".to_string()).unwrap();

        let loaded = PendingAnalysis::load(base).unwrap();
        assert_eq!(loaded.conversations, convs);
        assert_eq!(loaded.session_id, "sess-test-123");

        // 文件已被删除
        assert!(!base.join(PENDING_FILE).exists());
    }

    #[test]
    fn save_empty_conversations_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        PendingAnalysis::save(base, vec![], "sess-test".to_string()).unwrap();
        assert!(!base.join(PENDING_FILE).exists());
    }

    #[test]
    fn load_nonexistent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(PendingAnalysis::load(dir.path()).is_none());
    }

    #[test]
    fn format_for_injection_includes_conversations() {
        let pending = PendingAnalysis {
            conversations: vec!["对话1".to_string(), "对话2".to_string()],
            session_id: "sess-abc".to_string(),
            saved_at: "2026-05-08T00:00:00Z".to_string(),
        };
        let text = pending.format_for_injection();
        assert!(text.contains("对话1"));
        assert!(text.contains("对话2"));
        assert!(text.contains("sess-abc"));
    }

    #[test]
    fn format_truncates_long_conversation() {
        let long_conv: String = "x".repeat(600);
        let pending = PendingAnalysis {
            conversations: vec![long_conv],
            session_id: "sess-test".to_string(),
            saved_at: "2026-05-08".to_string(),
        };
        let text = pending.format_for_injection();
        // 应该被截断到 500 字 + "..."
        assert!(text.contains("..."));
        // 单条对话不应超过 510 字（500 + "..." + 序号）
        for line in text.lines() {
            if line.starts_with("1.") {
                assert!(line.chars().count() < 520);
            }
        }
    }
}
