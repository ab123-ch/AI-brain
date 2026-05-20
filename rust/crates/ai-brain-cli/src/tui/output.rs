//! 输出区域数据模型
//!
//! 核心设计:
//! - Spinner 只保留一行（新 spinner 替换旧的）
//! - 工具调用详情写入 SessionLogger，TUI 只显示汇总
//! - 模型回复默认折叠，可展开

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

use super::session_logger::SessionLogger;

// ─── 输出行类型 ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum OutputLine {
    UserInput(String),
    AssistantReply {
        text: String,
        expanded: bool,
        /// 思考内容（已被解析分离）
        thinking: Option<String>,
        /// 思考内容是否可见（默认隐藏）
        thinking_visible: bool,
    },
    ToolStart {
        name: String,
    },
    ToolDone {
        name: String,
        duration_ms: u64,
        is_error: bool,
    },
    ToolSummary {
        count: usize,
        total_ms: u64,
        has_error: bool,
    },
    MemoryInjected {
        count: usize,
    },
    /// 记忆详情（verbose 模式可见）
    MemoryDetail {
        memories: Vec<String>,
    },
    EvalResult {
        passed: bool,
        feedback: String,
    },
    /// 评估详情（verbose 模式可见）
    EvalDetail {
        score: f64,
        reports: Vec<String>,
        instructions: Vec<String>,
    },
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
    /// 当前流式思考内容缓冲区（模型推理时持续累积）
    streaming_thinking: String,
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
    /// Verbose 模式：显示记忆详情、评估详情、思考内容
    pub verbose: bool,
}

impl OutputArea {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            spinner_label: None,
            spinner_frame_char: String::new(),
            streaming_buf: String::new(),
            streaming_thinking: String::new(),
            streaming_flushed: false,
            tool_count: 0,
            tool_total_ms: 0,
            tool_has_error: false,
            session_logger: SessionLogger::new(),
            manual_scroll: 0,
            verbose: false,
        }
    }

    /// 处理一个 ProgressEvent
    pub fn handle_event(&mut self, event: &ProgressEvent) {
        self.session_logger.log_event(event);

        match event {
            ProgressEvent::Connecting { brain, model } => {
                let name = brain_display_name(brain);
                self.flush_streaming();
                self.spinner_label = Some(format!("{name}-连接中... ({model})"));
            }
            ProgressEvent::Thinking { brain } => {
                let name = brain_display_name(brain);
                self.flush_streaming();
                self.spinner_label = Some(format!("{name}-推理中..."));
            }
            ProgressEvent::TextDelta { text } => {
                // 纯文本内容，直接累积到 streaming_buf
                if !text.is_empty() {
                    self.streaming_buf.push_str(text);
                    self.streaming_flushed = false;
                }
            }
            ProgressEvent::ThinkingDelta { content } => {
                // 思考内容走专用通道，默认不显示，只缓存
                if !content.is_empty() {
                    self.streaming_thinking.push_str(content);
                    self.streaming_thinking.push('\n');
                }
            }
            ProgressEvent::ToolStart { tool_name, .. } => {
                self.flush_streaming();
                self.tool_count += 1;
                // 需求2: 显示每个工具调用的名称
                self.lines.push(OutputLine::ToolStart {
                    name: tool_name.clone(),
                });
            }
            ProgressEvent::ToolDone {
                tool_name,
                duration_ms,
                is_error,
                ..
            } => {
                // 需求2: 显示工具完成状态
                self.lines.push(OutputLine::ToolDone {
                    name: tool_name.clone(),
                    duration_ms: *duration_ms,
                    is_error: *is_error,
                });
                self.tool_total_ms += duration_ms;
                if *is_error {
                    self.tool_has_error = true;
                }
            }
            ProgressEvent::MemoryInjected { count, .. } => {
                self.flush_streaming();
                self.spinner_label = None;
                // 需求4: 记忆注入可见
                self.lines
                    .push(OutputLine::MemoryInjected { count: *count });
            }
            ProgressEvent::MemoryDetail { memories } => {
                self.lines.push(OutputLine::MemoryDetail {
                    memories: memories.clone(),
                });
            }
            ProgressEvent::EvaluationStart => {
                self.flush_streaming();
                // 需求3: 评估脑可见
                self.spinner_label = Some("评估脑-检查中...".into());
            }
            ProgressEvent::EvaluationResult { passed, feedback } => {
                self.spinner_label = None;
                // 需求3: 评估结果可见
                self.lines.push(OutputLine::EvalResult {
                    passed: *passed,
                    feedback: feedback.clone(),
                });
            }
            ProgressEvent::EvaluationDetail {
                score,
                reports,
                instructions,
            } => {
                let report_lines: Vec<String> = reports
                    .iter()
                    .map(|r| {
                        format!(
                            "[{}] health={:.0}% usage={:.0}%",
                            r.brain_id,
                            r.health_score * 100.0,
                            r.usage_percent * 100.0
                        )
                    })
                    .collect();
                let instr_lines: Vec<String> =
                    instructions.iter().map(|i| format!("{i:?}")).collect();
                self.lines.push(OutputLine::EvalDetail {
                    score: *score,
                    reports: report_lines,
                    instructions: instr_lines,
                });
            }
            ProgressEvent::Evaluating => {
                self.flush_streaming();
                self.spinner_label = Some("主脑-评估答案中...".into());
            }
            ProgressEvent::LlmRetry { .. } => {}
            ProgressEvent::AskUser { question, .. } => {
                self.flush_streaming();
                self.lines.push(OutputLine::ToolStart {
                    name: "AskUserQuestion".into(),
                });
                self.lines.push(OutputLine::System(format!(
                    "等待用户回答: {question}"
                )));
                self.spinner_label = Some("等待用户回答...".into());
            }
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

        // 去重：如果 flush_streaming 已推送相同文本，不重复添加
        // 注意：Done 事件会在 flush_streaming 之后追加 ToolSummary/Blank，
        // 所以不能只看 lines.last()，需要倒序查找最近的 AssistantReply
        for line in self.lines.iter().rev().take(5) {
            if let OutputLine::AssistantReply { text: prev_text, .. } = line {
                // 完全匹配 或 包含关系（streaming 可能只推送了部分）
                if prev_text.trim() == text.trim()
                    || text.len() > 100 && prev_text.contains(&text.trim())
                    || prev_text.len() > 100 && text.contains(prev_text.trim())
                {
                    return;
                }
            }
        }

        self.lines.push(OutputLine::AssistantReply {
            text: text.trim().to_string(),
            expanded: true,
            thinking: None, // thinking 已通过 ThinkingDelta 通道单独处理
            thinking_visible: false,
        });

    }


    pub fn push_system(&mut self, text: &str) {
        self.lines.push(OutputLine::System(text.to_string()));
    }

    /// 切换 Verbose 模式（Ctrl+E）：显示记忆详情、评估详情、思考内容
    pub fn toggle_verbose(&mut self) {
        self.verbose = !self.verbose;
        // 同步切换所有回复的 thinking_visible
        for line in &mut self.lines {
            if let OutputLine::AssistantReply {
                thinking_visible, ..
            } = line
            {
                *thinking_visible = self.verbose;
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

    /// 获取流式思考内容的最新一行（用于实时展示）
    pub fn streaming_thinking_latest_line(&self) -> Option<&str> {
        if self.streaming_thinking.is_empty() {
            return None;
        }
        // 取最后一个非空行
        self.streaming_thinking
            .lines()
            .filter(|l| !l.trim().is_empty())
            .last()
    }

    fn flush_streaming(&mut self) {
        if self.streaming_flushed {
            return;
        }
        // 将流式文本推入 lines 作为 AssistantReply
        // thinking 内容已在 streaming_thinking 中（通过 ThinkingDelta 事件），不走 TextDelta
        if !self.streaming_buf.trim().is_empty() {
            // 取出 thinking 缓存（如有）
            let thinking = if self.streaming_thinking.trim().is_empty() {
                None
            } else {
                Some(std::mem::take(&mut self.streaming_thinking))
            };
            self.lines.push(OutputLine::AssistantReply {
                text: self.streaming_buf.trim().to_string(),
                expanded: true,
                thinking,
                thinking_visible: self.verbose,
            });
        }
        self.streaming_buf.clear();
        self.streaming_thinking.clear();
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

    /// 将 manual_scroll 限制在 [0, max_scroll] 范围内
    pub fn clamp_scroll(&mut self, max_scroll: u16) {
        if self.manual_scroll > max_scroll {
            self.manual_scroll = max_scroll;
        }
    }
}

impl Default for OutputArea {
    fn default() -> Self {
        Self::new()
    }
}
