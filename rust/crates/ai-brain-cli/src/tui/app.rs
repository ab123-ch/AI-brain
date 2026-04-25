//! TUI 主应用组件
//!
//! 修复清单:
//! - Spinner 不再累积到 lines 中，只保留一个独立状态
//! - 输出区域自动滚到底部，Shift+↑/↓ 滚动
//! - `e` 键只在无文本输入时生效（通过 Ctrl+E 触发展开）
//! - finish_query 不阻塞事件循环

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;
use tui_textarea::Input;

use brain_core::types::ProgressEvent;

use crate::orchestrator::Orchestrator;

use super::input::InputArea;
use super::output::{OutputArea, OutputLine};
use super::status::StatusBar;

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⼾", "⼿", "⾀", "⾁", "⾂", "⾃"];
const COLLAPSE_MAX_CHARS: usize = 80;

pub struct App {
    output: OutputArea,
    status: StatusBar,
    input: InputArea,
    spinner_frame: usize,
    should_quit: bool,
    is_busy: bool,
    orch: Arc<Orchestrator>,
    progress_rx: Option<tokio::sync::mpsc::Receiver<ProgressEvent>>,
    query_handle:
        Option<tokio::task::JoinHandle<Result<brain_core::types::MainBrainOutput, String>>>,
    /// Done 事件已收到，等待 query_handle 完成
    done_received: bool,
    /// 待发送消息队列（busy 时 Enter 提交的消息排队等处理）
    pending_queue: Vec<String>,
}

impl App {
    pub fn new(orch: Orchestrator) -> Self {
        let orch = Arc::new(orch);
        let status_structured = orch.status_structured();
        let mut output = OutputArea::new();
        output.push_system("AI Brain v2 一主二从系统");
        output.push_system("输入查询 | :help 命令 | Ctrl+E 展开/收起回复");

        Self {
            output,
            status: StatusBar::from_system_status(&status_structured),
            input: InputArea::new(),
            spinner_frame: 0,
            should_quit: false,
            is_busy: false,
            orch,
            progress_rx: None,
            query_handle: None,
            done_received: false,
            pending_queue: Vec::new(),
        }
    }

    /// 主事件循环
    pub async fn run<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> std::io::Result<()> {
        let ctrl_c_flag = Arc::new(AtomicBool::new(false));
        let cc_flag = ctrl_c_flag.clone();
        let ctrl_c_task = tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            cc_flag.store(true, Ordering::Relaxed);
        });

        while !self.should_quit {
            // 1. 处理待显示结果
            self.flush_pending_result().await;

            // 2. 渲染
            terminal.draw(|f| self.render(f))?;

            // 3. 处理键盘/鼠标事件
            while event::poll(Duration::from_millis(50))? {
                if ctrl_c_flag.load(Ordering::Relaxed) {
                    self.handle_ctrl_c();
                    ctrl_c_flag.store(false, Ordering::Relaxed);
                    break;
                }

                match event::read()? {
                    Event::Key(key) => {
                        if self.handle_key(key) {
                            break;
                        }
                    }
                    Event::Mouse(mouse) => {
                        self.handle_mouse(mouse);
                    }
                    Event::Paste(text) => {
                        self.input.insert_paste(&text);
                    }
                    _ => {}
                }
            }

            if ctrl_c_flag.load(Ordering::Relaxed) {
                self.handle_ctrl_c();
                ctrl_c_flag.store(false, Ordering::Relaxed);
            }

            // 4. 处理进度事件（非阻塞）
            self.process_progress_events();

            // 5. spinner 动画
            if self.is_busy {
                self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
                self.output.tick_spinner(SPINNER_FRAMES[self.spinner_frame]);
            }
        }

        ctrl_c_task.abort();

        self.output.push_system("正在保存记忆（触发四步分析）...");
        terminal.draw(|f| self.render(f))?;

        let shutdown_done = Arc::new(AtomicBool::new(false));
        let done_flag = shutdown_done.clone();
        let guard = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            if done_flag.load(Ordering::Relaxed) {
                return;
            }
            eprintln!("\n正在保存记忆，请稍候...（再按一次强制退出）");
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            if !done_flag.load(Ordering::Relaxed) {
                std::process::exit(1);
            }
        });

        self.orch.shutdown_with_analysis().await;
        shutdown_done.store(true, Ordering::Relaxed);
        guard.abort();

        self.output.push_system("记忆已保存。再见！");
        terminal.draw(|f| self.render(f))?;

        Ok(())
    }

    // ─── 渲染 ──────────────────────────────────────────────────────

    fn render(&self, f: &mut ratatui::Frame) {
        let size = f.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(10),
                Constraint::Length(1),
                Constraint::Length(3),
            ])
            .split(size);

        self.render_output(f, chunks[0]);
        self.status.render(f, chunks[1]);
        f.render_widget(&self.input.textarea, chunks[2]);
    }

    fn render_output(&self, f: &mut ratatui::Frame, area: Rect) {
        let mut ratatui_lines: Vec<Line> = Vec::new();

        for line in &self.output.lines {
            // Verbose-only 行在非 verbose 模式下跳过
            match line {
                OutputLine::MemoryDetail { .. } | OutputLine::EvalDetail { .. } => {
                    if !self.output.verbose {
                        continue;
                    }
                }
                _ => {}
            }
            ratatui_lines.extend(self.format_output_line(line));
        }

        // 流式缓冲区（正在接收的文本）— 过滤思考标签
        if let Some(text) = self.output.streaming_text() {
            let (clean, _thinking) = crate::tui::output::OutputArea::strip_thinking_tags(text);
            if !clean.is_empty() {
                ratatui_lines.extend(self.format_output_line(&OutputLine::AssistantReply {
                    text: clean,
                    expanded: true,
                    thinking: None,
                    thinking_visible: false,
                }));
            }
        }

        // Spinner 行（独立渲染，不在 lines 中）
        if let Some(spinner) = self.output.spinner_text() {
            ratatui_lines.push(Line::from(Span::styled(
                format!("  {spinner}"),
                Style::default().fg(Color::Blue),
            )));
        }

        // 滚动计算：考虑文本换行后的实际行数
        let area_width = area.width as usize;
        let actual_lines: usize = ratatui_lines
            .iter()
            .map(|l| {
                let line_width = l.width();
                if line_width == 0 {
                    1
                } else {
                    (line_width + area_width - 1) / area_width
                }
            })
            .sum();
        let visible_lines = area.height as usize;
        let max_scroll = actual_lines.saturating_sub(visible_lines) as u16;
        let scroll = if self.output.manual_scroll > 0 {
            // 手动模式：用户在往上翻，从底部往上偏移
            max_scroll.saturating_sub(self.output.manual_scroll)
        } else {
            // 自动模式：始终显示最新内容
            max_scroll
        };

        let paragraph = Paragraph::new(ratatui_lines)
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

        f.render_widget(paragraph, area);
    }

    #[allow(clippy::too_many_lines)]
    fn format_output_line(&self, line: &OutputLine) -> Vec<Line<'static>> {
        match line {
            OutputLine::UserInput(text) => {
                vec![Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(text.clone(), Style::default().fg(Color::White)),
                ])]
            }

            OutputLine::AssistantReply {
                text,
                expanded,
                thinking,
                thinking_visible,
            } => {
                let mut lines = Vec::new();

                if *expanded {
                    // 显示展开标记
                    lines.push(Line::from(Span::styled(
                        "▾",
                        Style::default().fg(Color::DarkGray),
                    )));

                    // 显示正常内容
                    for l in text.lines() {
                        lines.push(Line::from(Span::styled(
                            l.to_string(),
                            Style::default().fg(Color::White),
                        )));
                    }

                    // 显示思考内容（如果可见）
                    if let Some(thinking_content) = thinking {
                        if *thinking_visible {
                            // 思考内容可见，显示分隔线和思考内容
                            lines.push(Line::from(Span::styled(
                                "  ────── 思考内容 ──────",
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::DIM),
                            )));
                            for l in thinking_content.lines() {
                                lines.push(Line::from(Span::styled(
                                    format!("  {l}"),
                                    Style::default()
                                        .fg(Color::DarkGray)
                                        .add_modifier(Modifier::DIM),
                                )));
                            }
                            lines.push(Line::from(Span::styled(
                                "▴ [Ctrl+E 隐藏思考]",
                                Style::default().fg(Color::DarkGray),
                            )));
                        } else {
                            // 思考内容隐藏，显示提示
                            lines.push(Line::from(Span::styled(
                                "▸ [Ctrl+E 查看思考]",
                                Style::default().fg(Color::DarkGray),
                            )));
                        }
                    }
                } else {
                    // 折叠状态（不再使用，但保留兼容）
                    let first_line = text.lines().next().unwrap_or("");
                    let display = if first_line.chars().count() > COLLAPSE_MAX_CHARS {
                        let truncated: String =
                            first_line.chars().take(COLLAPSE_MAX_CHARS).collect();
                        format!("{truncated}...")
                    } else if text.lines().count() > 1 {
                        format!("{first_line} ...")
                    } else {
                        first_line.to_string()
                    };
                    lines.push(Line::from(vec![
                        Span::styled("▸ ", Style::default().fg(Color::DarkGray)),
                        Span::styled(display, Style::default().fg(Color::White)),
                        Span::styled(
                            format!(" [{}行]", text.lines().count()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }

                lines
            }

            OutputLine::ToolStart { name } => {
                vec![Line::from(Span::styled(
                    format!("  ⏳ {name}..."),
                    Style::default().fg(Color::DarkGray),
                ))]
            }

            OutputLine::ToolDone {
                name,
                duration_ms,
                is_error,
            } => {
                let dur = format_duration(*duration_ms);
                if *is_error {
                    vec![Line::from(Span::styled(
                        format!("  ✘ {name} ({dur}) — 失败"),
                        Style::default().fg(Color::Yellow),
                    ))]
                } else {
                    vec![Line::from(Span::styled(
                        format!("  ✔ {name} ({dur})"),
                        Style::default().fg(Color::Green),
                    ))]
                }
            }

            OutputLine::ToolSummary {
                count,
                total_ms,
                has_error,
            } => {
                let dur = format_duration(*total_ms);
                let err = if *has_error { " (有错误)" } else { "" };
                vec![Line::from(Span::styled(
                    format!("  🔧 {count} tools | {dur}{err}"),
                    Style::default().fg(Color::DarkGray),
                ))]
            }

            OutputLine::MemoryInjected { count } => {
                vec![Line::from(Span::styled(
                    format!("  🧠 记忆注入: {count} 条"),
                    Style::default().fg(Color::Cyan),
                ))]
            }

            OutputLine::EvalResult {
                passed,
                issue_count,
            } => {
                if *passed {
                    vec![Line::from(Span::styled(
                        "  评估 ✔ 通过",
                        Style::default().fg(Color::Green),
                    ))]
                } else {
                    vec![Line::from(Span::styled(
                        format!("  评估 ✘ {issue_count} 个问题"),
                        Style::default().fg(Color::Yellow),
                    ))]
                }
            }

            OutputLine::System(text) => {
                vec![Line::from(Span::styled(
                    text.clone(),
                    Style::default().fg(Color::DarkGray),
                ))]
            }

            OutputLine::MemoryDetail { memories } => {
                let mut lines = Vec::new();
                lines.push(Line::from(Span::styled(
                    "  │ ────── 记忆详情 ──────",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                )));
                for (i, mem) in memories.iter().enumerate() {
                    lines.push(Line::from(Span::styled(
                        format!("  │ {}. {}", i + 1, mem),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines
            }

            OutputLine::EvalDetail {
                score,
                reports,
                instructions,
            } => {
                let mut lines = Vec::new();
                lines.push(Line::from(Span::styled(
                    format!("  │ ────── 评估详情 ({:.0}%) ──────", score * 100.0),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                )));
                for r in reports {
                    lines.push(Line::from(Span::styled(
                        format!("  │ {r}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                if !instructions.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "  │ 瘦身指令:",
                        Style::default().fg(Color::DarkGray),
                    )));
                    for instr in instructions {
                        lines.push(Line::from(Span::styled(
                            format!("  │   {instr}"),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                }
                lines
            }

            OutputLine::Blank => {
                vec![Line::from("")]
            }
        }
    }

    // ─── 事件处理 ──────────────────────────────────────────────────

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        match (key.modifiers, key.code) {
            // Ctrl+C / Esc — 取消查询或退出
            (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => {
                if self.is_busy {
                    self.cancel_query();
                    return true;
                }
                self.should_quit = true;
                return true;
            }
            // Ctrl+D — 退出
            (KeyModifiers::CONTROL, KeyCode::Char('d')) => {
                self.should_quit = true;
                return true;
            }
            // Ctrl+E — 切换 Verbose 模式（思考+记忆+评估详情）
            (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                self.output.toggle_verbose();
                return true;
            }
            // Shift+↑ / PageUp — 向上滚动
            (KeyModifiers::SHIFT, KeyCode::Up) | (_, KeyCode::PageUp) => {
                self.output.scroll_up(5);
                return true;
            }
            // Shift+↓ / PageDown — 向下滚动
            (KeyModifiers::SHIFT, KeyCode::Down) | (_, KeyCode::PageDown) => {
                self.output.scroll_down(5);
                return true;
            }
            _ => {}
        }

        // Enter 提交：busy 时排队，否则直接发送
        if key.code == KeyCode::Enter {
            self.submit_input();
            return true;
        }

        // 其他键（打字、方向键等）始终交给 textarea，不受 busy 限制
        let input: Input = key.into();
        self.input.textarea.input(input);
        true
    }

    fn submit_input(&mut self) {
        let text = self.input.submit();
        if text.is_empty() {
            return;
        }

        // 内置命令始终立即处理
        match self.handle_builtin_command_sync(&text) {
            CommandResult::Handled => return,
            CommandResult::Exit => {
                self.should_quit = true;
                return;
            }
            CommandResult::Unknown => {}
        }

        self.output.push_user_input(&text);

        if self.is_busy {
            // busy 时排队，等当前回复完成后再发
            self.pending_queue.push(text);
            self.output.push_system(&format!(
                "  (已排队，等待当前回复完成... 队列: {})",
                self.pending_queue.len()
            ));
        } else {
            self.start_query(&text);
        }
    }

    /// 发起一次查询
    fn start_query(&mut self, text: &str) {
        let (rx, handle) = Arc::clone(&self.orch).query_streaming(text);
        self.progress_rx = Some(rx);
        self.query_handle = Some(handle);
        self.is_busy = true;
        self.status.busy = true;
        self.status.round = self.orch.status_structured().query_count;
    }

    /// 非阻塞处理进度事件
    fn process_progress_events(&mut self) {
        if let Some(ref mut rx) = self.progress_rx {
            loop {
                match rx.try_recv() {
                    Ok(event) => {
                        if !self.output.is_manual_scrolling() {
                            self.output.reset_auto_scroll();
                        }
                        if matches!(event, ProgressEvent::Done) {
                            self.output.handle_event(&ProgressEvent::Done);
                            self.try_collect_result();
                            return;
                        }
                        self.output.handle_event(&event);
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        self.try_collect_result();
                        return;
                    }
                }
            }
        }
    }

    /// 非阻塞标记 Done 已收到
    fn try_collect_result(&mut self) {
        self.done_received = true;
    }

    /// 如果 Done 已收到且 handle 完成，await 结果并显示
    async fn flush_pending_result(&mut self) {
        if !self.done_received {
            return;
        }
        if let Some(handle) = self.query_handle.as_ref() {
            if !handle.is_finished() {
                // handle 还没完成，下次循环再试
                return;
            }
        }

        self.done_received = false;
        self.is_busy = false;
        self.status.busy = false;
        self.progress_rx = None;

        if let Some(handle) = self.query_handle.take() {
            match handle.await {
                Ok(Ok(output)) => {
                    self.output
                        .push_assistant_reply(&output.answer, output.usage.duration_ms);
                }
                Ok(Err(e)) => {
                    self.output.push_system(&format!("错误: {e}"));
                }
                Err(_) => {}
            }
        }

        // 检查队列，有待发消息就自动发
        let next = self.pending_queue.first().cloned();
        if let Some(text) = next {
            self.pending_queue.remove(0);
            self.start_query(&text);
        }
    }

    fn cancel_query(&mut self) {
        if let Some(handle) = self.query_handle.take() {
            handle.abort();
        }
        self.progress_rx = None;
        self.done_received = false;
        self.is_busy = false;
        self.status.busy = false;
        self.pending_queue.clear();
        self.output.push_system("查询已取消");
    }

    fn handle_ctrl_c(&mut self) {
        if self.is_busy {
            self.cancel_query();
        } else {
            self.should_quit = true;
        }
    }

    /// 处理鼠标事件（滚轮滚动）
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.output.scroll_up(3);
            }
            MouseEventKind::ScrollDown => {
                self.output.scroll_down(3);
            }
            _ => {}
        }
    }

    /// 处理内置命令（同步版本，不 await 编排器的异步方法）
    fn handle_builtin_command_sync(&mut self, input: &str) -> CommandResult {
        match input {
            ":help" | "help" => {
                for line in [
                    "=== AI Brain v2 命令 ===",
                    "  :help           — 显示帮助",
                    "  :status         — 系统状态",
                    "  :memory         — 记忆统计",
                    "  :quit / Ctrl+C  — 退出并保存",
                    "  Ctrl+E          — 显示/隐藏详情（思考+记忆+评估）",
                    "  Shift+↑/↓       — 上下滚动输出",
                    "  ↑/↓             — 翻阅输入历史",
                ] {
                    self.output.push_system(line);
                }
                CommandResult::Handled
            }
            ":status" | "status" => {
                self.output.push_system(&self.orch.status());
                CommandResult::Handled
            }
            ":quit" | ":exit" | "exit" | "quit" => CommandResult::Exit,
            _ => CommandResult::Unknown,
        }
    }
}

enum CommandResult {
    Handled,
    Exit,
    Unknown,
}

fn format_duration(duration_ms: u64) -> String {
    if duration_ms < 1000 {
        format!("{duration_ms}ms")
    } else {
        let secs = duration_ms / 1000;
        let frac = duration_ms % 1000 / 100;
        format!("{secs}.{frac}s")
    }
}
