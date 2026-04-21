//! 输出区域数据模型
//!
//! 核心设计:
//! - Spinner 只保留一行（新 spinner 替换旧的）
//! - 工具调用详情写入 SessionLogger，TUI 只显示汇总
//! - 模型回复默认折叠，可展开

use brain_core::types::ProgressEvent;

use crate::terminal::brain_display_name;

use super::session_logger::SessionLogger;

// ─── 输出行类型 ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum OutputLine {
    UserInput(String),
    AssistantReply { text: String, expanded: bool },
    ToolSummary { count: usize, total_ms: u64, has_error: bool },
    MemoryInjected { count: usize },
    EvalResult { passed: bool, issue_count: usize },
    System(String),
    Blank,
}

// ─── 输出区域 ──────────────────────────────────────────────────────

pub struct OutputArea {
    /// 所有已完成的输出行（不含 spinner）
    pub lines: Vec<OutputLine>,
    /// 当前 spinner 标签（只保留一个，不在 lines 里）
    spinner_label: Option<String>,
    /// spinner 帧字符
    spinner_frame_char: String,
    /// 当前流式文本缓冲区
    streaming_buf: String,
    /// 是否有流式文本已经刷入 lines 的标记（避免重复刷入）
    streaming_flushed: bool,
    /// 当前轮工具统计
    tool_count: usize,
    tool_total_ms: u64,
    tool_has_error: bool,
    /// 会话日志
    session_logger: SessionLogger,
    /// 手动滚动偏移（0 = 自动滚底）
    pub manual_scroll: u16,
}

impl OutputArea {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            spinner_label: None,
            spinner_frame_char: String::new(),
            streaming_buf: String::new(),
            streaming_flushed: false,
            tool_count: 0,
            tool_total_ms: 0,
            tool_has_error: false,
            session_logger: SessionLogger::new(),
            manual_scroll: 0,
        }
    }

    /// 处理一个 ProgressEvent
    pub fn handle_event(&mut self, event: &ProgressEvent) {
        self.session_logger.log_event(event);

        match event {
            ProgressEvent::Connecting { brain, model } => {
                let name = brain_display_name(brain);
                self.flush_streaming();
                // 只更新 spinner 标签，不往 lines 里添加
                self.spinner_label = Some(format!("{name}-连接中... ({model})"));
            }
            ProgressEvent::Thinking { brain } => {
                let name = brain_display_name(brain);
                self.flush_streaming();
                self.spinner_label = Some(format!("{name}-推理中..."));
            }
            ProgressEvent::TextDelta { text } => {
                self.streaming_buf.push_str(text);
                self.streaming_flushed = false;
            }
            ProgressEvent::ToolStart { .. } => {
                self.flush_streaming();
                self.tool_count += 1;
            }
            ProgressEvent::ToolDone { duration_ms, is_error, .. } => {
                self.tool_total_ms += duration_ms;
                if *is_error {
                    self.tool_has_error = true;
                }
            }
            ProgressEvent::MemoryInjected { count, .. } => {
                self.flush_streaming();
                self.spinner_label = None;
                self.lines.push(OutputLine::MemoryInjected { count: *count });
            }
            ProgressEvent::EvaluationStart => {
                self.flush_streaming();
                self.spinner_label = Some("评估脑-检查中...".into());
            }
            ProgressEvent::EvaluationResult { passed, issues } => {
                self.spinner_label = None;
                self.lines.push(OutputLine::EvalResult {
                    passed: *passed,
                    issue_count: issues.len(),
                });
            }
            ProgressEvent::Evaluating => {
                self.flush_streaming();
                self.spinner_label = Some("主脑-评估答案中...".into());
            }
            ProgressEvent::LlmRetry { .. } => {}
            ProgressEvent::Done => {
                self.flush_streaming();
                self.spinner_label = None;

                if self.tool_count > 0 {
                    self.lines.push(OutputLine::ToolSummary {
                        count: self.tool_count,
                        total_ms: self.tool_total_ms,
                        has_error: self.tool_has_error,
                    });
                }

                self.tool_count = 0;
                self.tool_total_ms = 0;
                self.tool_has_error = false;
                self.lines.push(OutputLine::Blank);
            }
        }
    }

    pub fn push_user_input(&mut self, text: &str) {
        self.session_logger.log_user_input(text);
        self.lines.push(OutputLine::UserInput(text.to_string()));
    }

    pub fn push_assistant_reply(&mut self, text: &str, duration_ms: u64) {
        self.session_logger.log_assistant_reply(text, duration_ms);
        if text.trim().is_empty() {
            return;
        }
        self.lines.push(OutputLine::AssistantReply {
            text: text.to_string(),
            expanded: false,
        });
    }

    pub fn push_system(&mut self, text: &str) {
        self.lines.push(OutputLine::System(text.to_string()));
    }

    /// 切换最近一条回复的展开/折叠
    pub fn toggle_last_reply_expand(&mut self) {
        for i in (0..self.lines.len()).rev() {
            if let OutputLine::AssistantReply { expanded, .. } = &mut self.lines[i] {
                *expanded = !*expanded;
                return;
            }
        }
    }

    /// 更新 spinner 帧字符
    pub fn tick_spinner(&mut self, frame_char: &str) {
        self.spinner_frame_char = frame_char.to_string();
    }

    /// 获取当前 spinner 显示文本（如有）
    pub fn spinner_text(&self) -> Option<String> {
        self.spinner_label
            .as_ref()
            .map(|label| format!("{} {}", self.spinner_frame_char, label))
    }

    /// 获取流式缓冲区内容
    pub fn streaming_text(&self) -> Option<&str> {
        if self.streaming_buf.is_empty() {
            None
        } else {
            Some(&self.streaming_buf)
        }
    }

    fn flush_streaming(&mut self) {
        // 清空 streaming_buf 但不推入 lines
        // 最终回答由 finish_query 中的 push_assistant_reply 推入
        if self.streaming_flushed {
            return;
        }
        self.streaming_buf.clear();
        self.streaming_flushed = true;
    }

    /// 向上滚（查看更早的内容）
    pub fn scroll_up(&mut self, amount: u16) {
        self.manual_scroll = self.manual_scroll.saturating_add(amount);
    }

    /// 向下滚（查看更新的内容）
    pub fn scroll_down(&mut self, amount: u16) {
        self.manual_scroll = self.manual_scroll.saturating_sub(amount);
    }

    /// 新内容到达时重置为自动滚底
    pub fn reset_auto_scroll(&mut self) {
        self.manual_scroll = 0;
    }

    /// 是否处于手动滚动模式
    pub fn is_manual_scrolling(&self) -> bool {
        self.manual_scroll > 0
    }
}

impl Default for OutputArea {
    fn default() -> Self {
        Self::new()
    }
}
