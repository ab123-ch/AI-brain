//! 会话日志记录器
//!
//! 将工具调用详情写入独立日志文件（一个会话一个文件），
//! 不在 TUI 交互区域展示，保持界面干净。
//!
//! 日志路径: ~/.ai-brain/sessions/{session_id}.log

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use brain_core::types::ProgressEvent;

/// 脑名映射（内联避免跨模块引用）
fn brain_display_name(brain: &str) -> &str {
    match brain {
        "main" => "主脑",
        "memory" => "记忆脑",
        "eval" => "评估脑",
        _ => brain,
    }
}

/// 会话日志记录器
pub struct SessionLogger {
    /// 日志文件路径
    #[allow(dead_code)]
    path: PathBuf,
    /// 文件句柄（Mutex 保证线程安全）
    writer: Mutex<File>,
}

impl SessionLogger {
    /// 创建新的会话日志
    ///
    /// 日志文件: `~/.ai-brain/sessions/{YYYYMMDD-HHMMSS}.log`
    pub fn new() -> Self {
        let base = crate::init::base_dir();
        let sessions_dir = base.join("sessions");
        let _ = fs::create_dir_all(&sessions_dir);

        let session_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let path = sessions_dir.join(format!("{session_id}.log"));

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| {
                eprintln!("无法创建会话日志 {}: {e}", path.display());
                std::process::exit(1);
            });

        // 写入会话头
        let sl = Self {
            path,
            writer: Mutex::new(file),
        };
        sl.log_raw(&format!(
            "═══ AI Brain v2 会话日志 ═══ {} ═══\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        ));
        sl
    }

    /// 处理进度事件，将工具相关事件写入日志
    pub fn log_event(&self, event: &ProgressEvent) {
        match event {
            ProgressEvent::ToolStart {
                brain,
                tool_name,
                input,
                ..
            } => {
                let name = brain_display_name(brain);
                let ts = Self::timestamp();
                self.log_raw(&format!(
                    "[{ts}] TOOL_START | {name} | {tool_name}\n  input: {input}\n"
                ));
            }
            ProgressEvent::ToolDone {
                brain,
                tool_name,
                duration_ms,
                output_preview,
                is_error,
                ..
            } => {
                let name = brain_display_name(brain);
                let status = if *is_error { "FAIL" } else { "OK" };
                let ts = Self::timestamp();
                self.log_raw(&format!(
                    "[{ts}] TOOL_{status} | {name} | {tool_name} | {duration_ms}ms\n  output: {output_preview}\n"
                ));
            }
            ProgressEvent::LlmRetry {
                attempt,
                max_attempts,
                error,
            } => {
                let ts = Self::timestamp();
                self.log_raw(&format!(
                    "[{ts}] LLM_RETRY | {attempt}/{max_attempts} | {error}\n"
                ));
            }
            ProgressEvent::Connecting { brain, model } => {
                let name = brain_display_name(brain);
                let ts = Self::timestamp();
                self.log_raw(&format!("[{ts}] CONNECT | {name} | {model}\n"));
            }
            ProgressEvent::IntermediateConclusion { brain, content } => {
                let name = brain_display_name(brain);
                let ts = Self::timestamp();
                self.log_raw(&format!("[{ts}] CHECKPOINT | {name} | {content}\n"));
            }
            ProgressEvent::EvaluationStart => {
                let ts = Self::timestamp();
                self.log_raw(&format!("[{ts}] EVAL_START | 评估脑\n"));
            }
            ProgressEvent::EvaluationResult { passed, feedback } => {
                let ts = Self::timestamp();
                let status = if *passed { "PASS" } else { "FAIL" };
                let preview: String = feedback.chars().take(500).collect();
                self.log_raw(&format!(
                    "[{ts}] EVAL_RESULT | {status}\n  反馈: {preview}\n"
                ));
            }
            ProgressEvent::Evaluating => {
                let ts = Self::timestamp();
                self.log_raw(&format!("[{ts}] EVALUATING | 评估脑处理中...\n"));
            }
            ProgressEvent::AskUser { question, .. } => {
                let ts = Self::timestamp();
                self.log_raw(&format!("[{ts}] ASK_USER | {question}\n"));
            }
            _ => {}
        }
    }

    /// 记录用户输入
    pub fn log_user_input(&self, text: &str) {
        let ts = Self::timestamp();
        self.log_raw(&format!("[{ts}] USER_INPUT | {text}\n"));
    }

    /// 记录助手回复
    pub fn log_assistant_reply(&self, text: &str, duration_ms: u64) {
        let ts = Self::timestamp();
        // 截断到 500 字符，避免日志文件过大
        let preview: String = text.chars().take(500).collect();
        let ellipsis = if text.chars().count() > 500 {
            "..."
        } else {
            ""
        };
        self.log_raw(&format!(
            "[{ts}] ASSISTANT | {duration_ms}ms\n{preview}{ellipsis}\n\n"
        ));
    }

    /// 写入原始文本
    fn log_raw(&self, text: &str) {
        if let Ok(mut file) = self.writer.lock() {
            let _ = file.write_all(text.as_bytes());
            let _ = file.flush();
        }
    }

    fn timestamp() -> String {
        chrono::Local::now().format("%H:%M:%S%.3f").to_string()
    }
}
