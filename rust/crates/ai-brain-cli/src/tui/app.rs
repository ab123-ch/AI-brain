//! TUI 主应用组件
//!
//! 修复清单:
//! - Spinner 不再累积到 lines 中，只保留一个独立状态
//! - 输出区域自动滚到底部，Shift+↑/↓ 滚动
//! - `e` 键只在无文本输入时生效（通过 Ctrl+E 触发展开）
//! - finish_query 不阻塞事件循环

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use brain_core::types::ProgressEvent;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;

use crate::orchestrator::Orchestrator;

use super::input::{InputArea, InputResult};
use super::output::{OutputArea, OutputLine};
use super::status::StatusBar;
use super::completion::EvolutionCompleter;

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
    /// 上一次键盘事件时间
    last_event_time: Instant,
}

impl App {
    pub fn new(orch: Orchestrator) -> Self {
        let orch = Arc::new(orch);
        let status_structured = orch.status_structured();
        let mut output = OutputArea::new();
        output.push_system("AI Brain v2 一主二从系统");
        output.push_system("输入查询 | :help 命令 | Enter 提交 | Tab 补全 | ↑↓ 历史 | Shift+Enter 换行");

        // 从 orchestrator 获取 brain-evolution 数据
        let completer = {
            let (template_names, pattern_keywords) = orch.completion_data();
            EvolutionCompleter::new(template_names, pattern_keywords)
        };

        Self {
            output,
            status: StatusBar::from_system_status(&status_structured),
            input: InputArea::new(completer),
            spinner_frame: 0,
            should_quit: false,
            is_busy: false,
            orch,
            progress_rx: None,
            query_handle: None,
            done_received: false,
            pending_queue: Vec::new(),
            last_event_time: Instant::now(),
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

            // 2. 渲染（屏幕休眠时 draw 可能失败，容错跳过）
            if let Err(e) = terminal.draw(|f| self.render(f)) {
                tracing::debug!("TUI 渲染失败（可能屏幕休眠）: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }

            // 3. 处理键盘/鼠标事件（屏幕休眠时 poll 可能报错）
            match event::poll(Duration::from_millis(50)) {
                Ok(true) => {
                    loop {
                        if ctrl_c_flag.load(Ordering::Relaxed) {
                            self.handle_ctrl_c();
                            ctrl_c_flag.store(false, Ordering::Relaxed);
                            break;
                        }

                        match event::read() {
                            Ok(Event::Key(key)) => {
                                if self.handle_key(key) {
                                    break;
                                }
                            }
                            Ok(Event::Mouse(mouse)) => {
                                self.handle_mouse(mouse);
                            }
                            Ok(Event::Paste(text)) => {
                                self.input.insert_paste(&text);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::debug!("TUI 事件读取失败: {e}");
                                break;
                            }
                        }

                        // 检查是否还有待处理事件（非阻塞）
                        match event::poll(Duration::from_millis(0)) {
                            Ok(true) => continue,
                            _ => break,
                        }
                    }
                }
                Ok(false) => {} // 无事件，正常
                Err(e) => {
                    tracing::debug!("TUI 事件轮询失败（可能屏幕休眠）: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
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

            // 6. 让出 CPU，防止忙等待（屏幕休眠时尤为重要）
            tokio::task::yield_now().await;
        }

        ctrl_c_task.abort();

        self.output.push_system("正在保存会话数据...");
        terminal.draw(|f| self.render(f))?;

        self.orch.shutdown_with_analysis().await;

        self.output.push_system("会话数据已保存。再见！");
        terminal.draw(|f| self.render(f))?;

        Ok(())
    }

    // ─── 渲染 ──────────────────────────────────────────────────────

    fn render(&mut self, f: &mut ratatui::Frame) {
        let size = f.area();

        // 更新折行宽度（终端 resize 时自动适配）
        self.input.set_wrap_width(size.width);

        let input_h = self.input.desired_height();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(10),
                Constraint::Length(1),
                Constraint::Length(input_h),
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

        // 流式缓冲区（正在接收的文本）— 纯文本，thinking 已走 ThinkingDelta 通道
        if let Some(text) = self.output.streaming_text() {
            if !text.is_empty() {
                ratatui_lines.extend(self.format_output_line(&OutputLine::AssistantReply {
                    text: text.to_string(),
                    expanded: true,
                    thinking: None,
                    thinking_visible: false,
                }));
            }
        }

        // 流式思考内容：只显示最新一行，持续滚动
        if let Some(latest) = self.output.streaming_thinking_latest_line() {
            ratatui_lines.push(Line::from(Span::styled(
                format!("  💭 {latest}"),
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            )));
        }

        // Spinner 行（独立渲染，不在 lines 中）
        if let Some(spinner) = self.output.spinner_text() {
            ratatui_lines.push(Line::from(Span::styled(
                format!("  {spinner}"),
                Style::default().fg(Color::Blue),
            )));
        }

        // 滚动计算：考虑文本换行后的实际行数
        // ratatui Wrap 按词边界换行，可能比 ceil(width/area_width) 多产生视觉行
        // 策略：基础估算 + 换行缓冲 + 安全裕量
        let area_width = area.width as usize;
        let mut actual_lines = 0;
        for l in &ratatui_lines {
            let w = l.width();
            if w == 0 {
                actual_lines += 1;
            } else {
                // 每行至少占 1 行，宽度超过 area_width 时额外加上换行次数
                let base = (w + area_width - 1) / area_width;
                actual_lines += base;
                // 额外缓冲：ratatui 按词边界换行，可能产生更多行
                // 对于长行，每 80 字符额外加 1 行缓冲
                if w > area_width {
                    let extra_buffer = (w / 80).min(10);
                    actual_lines += extra_buffer;
                }
            }
        }
        // 安全裕量：防止估算不足
        actual_lines += 20;

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

                    // Markdown 渲染主内容
                    lines.extend(render_markdown_lines(text));

                    // 显示思考内容
                    if let Some(thinking_content) = thinking {
                        let all_lines: Vec<&str> = thinking_content.lines().collect();
                        let line_count = all_lines.len();

                        if *thinking_visible {
                            // 展开状态：显示分隔线 + 全部思考内容
                            lines.push(Line::from(Span::styled(
                                "  ────── 思考内容 ──────",
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::DIM),
                            )));
                            for l in &all_lines {
                                lines.push(Line::from(Span::styled(
                                    format!("  {l}"),
                                    Style::default()
                                        .fg(Color::DarkGray)
                                        .add_modifier(Modifier::DIM),
                                )));
                            }
                            lines.push(Line::from(Span::styled(
                                format!("▴ [{line_count}行 — Ctrl+E 折叠思考]"),
                                Style::default().fg(Color::DarkGray),
                            )));
                        } else {
                            // 折叠状态：只显示最新一行（最后一行非空）
                            lines.push(Line::from(Span::styled(
                                "  ────── 思考内容 ──────",
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::DIM),
                            )));
                            if let Some(last) = all_lines.iter().rev().find(|l| !l.trim().is_empty()) {
                                lines.push(Line::from(Span::styled(
                                    format!("  💭 {last}"),
                                    Style::default()
                                        .fg(Color::DarkGray)
                                        .add_modifier(Modifier::DIM),
                                )));
                            }
                            if line_count > 1 {
                                lines.push(Line::from(Span::styled(
                                    format!("▸ [共{}行 — Ctrl+E 展开全部]", line_count),
                                    Style::default().fg(Color::DarkGray),
                                )));
                            }
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

            OutputLine::EvalResult { passed, feedback } => {
                if *passed {
                    vec![Line::from(Span::styled(
                        "  评估 ✔ 通过",
                        Style::default().fg(Color::Green),
                    ))]
                } else {
                    // 显示评估脑反馈文本
                    let mut lines = vec![Line::from(Span::styled(
                        "  评估 ✘ 发现问题",
                        Style::default().fg(Color::Yellow),
                    ))];
                    for line in feedback.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("    {}", line),
                            Style::default().fg(Color::Yellow),
                        )));
                    }
                    lines
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
        let now = Instant::now();
        self.last_event_time = now;

        // ── App 层独占按键 ──
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
            // Ctrl+E — 切换 Verbose 模式
            (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                self.output.toggle_verbose();
                return true;
            }
            // Shift+↑ / PageUp — 向上滚动输出
            (KeyModifiers::SHIFT, KeyCode::Up) | (_, KeyCode::PageUp) => {
                self.output.scroll_up(5);
                return true;
            }
            // Shift+↓ / PageDown — 向下滚动输出
            (KeyModifiers::SHIFT, KeyCode::Down) | (_, KeyCode::PageDown) => {
                self.output.scroll_down(5);
                return true;
            }
            _ => {}
        }

        // ── 委托给 InputArea ──
        match self.input.apply_key(key) {
            InputResult::Submit => self.submit_input(),
            InputResult::Consumed => {}
            InputResult::Ignored => {
                // 未识别的按键：静默忽略（不写入 textarea，避免与 original 不同步）
            }
        }
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
        let status = self.orch.status_structured();
        self.status.busy = true;
        self.status.round = status.query_count;
        self.status.context_usage = status.context_usage as f32;
        self.status.cumulative_prompt_tokens = status.cumulative_prompt_tokens;
        self.status.cumulative_completion_tokens = status.cumulative_completion_tokens;
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

        // 查询完成后刷新状态（获取最新的累计 token 计数）
        let status = self.orch.status_structured();
        self.status.context_usage = status.context_usage as f32;
        self.status.cumulative_prompt_tokens = status.cumulative_prompt_tokens;
        self.status.cumulative_completion_tokens = status.cumulative_completion_tokens;

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
    /// 
    /// **文本选择兼容**：
    /// - 在 MouseCapture 开启时，终端原生选择功能被拦截
    /// - 大多数终端（iTerm2/Terminal.app/Alacritty）支持 **按住 Shift/Option** 来绕过 mouse tracking 进行选择
    /// - 当检测到 SHIFT 修饰键时，完全忽略鼠标事件：TUI 不处理滚轮，终端有机会处理选择/复制
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        // Shift 按下 → 用户在尝试终端原生选择/复制，不干扰
        if mouse.modifiers.contains(KeyModifiers::SHIFT) {
            return;
        }
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
                    "  :evo <目标>     — 启动进化任务",
                    "  :evo-status     — 查看进化状态",
                    "  :evo-approve    — 确认合并进化结果",
                    "  :evo-reject     — 拒绝并回滚进化",
                    "  :evo-diff       — 查看进化变更",
                    "  :quit / Ctrl+C  — 退出并保存",
                    "  Enter           — 提交消息",
                    "  Shift+Enter     — 插入换行",
                    "  Ctrl+E          — 显示/隐藏详情（思考+记忆+评估）",
                    "  Shift+↑/↓       — 上下滚动输出",
                    "  ↑/↓             — 翻阅输入历史",
                    "  Ctrl+W/Backspace— 删除前一词",
                    "  Ctrl+Delete     — 删除后一词",
                    "  Ctrl+U/K        — 删到行首/行尾",
                    "  Ctrl+←/→        — 词间跳转",
                    "  Ctrl+P          — 展开/折叠长粘贴",
                    "  Ctrl+A/E        — 跳到行首/行尾",
                    "  鼠标滚轮        — 上下滚动输出",
                    "  Shift+鼠标选择  — 按住Shift选择/复制 (iTerm2); macOS终端按住Option",
                ] {
                    self.output.push_system(line);
                }
                CommandResult::Handled
            }
            ":status" | "status" => {
                self.output.push_system(&self.orch.status());
                CommandResult::Handled
            }
            cmd if cmd == ":evo"
                || cmd.starts_with(":evo ")
                || matches!(
                    cmd,
                    ":evo-status" | ":evo-approve" | ":evo-reject" | ":evo-diff"
                ) =>
            {
                // 进化命令需要异步调用，TUI 同步方法中暂为占位提示
                let msg = match input {
                    c if c == ":evo" || c.starts_with(":evo ") => {
                        let goal = c.strip_prefix(":evo").unwrap_or("").trim();
                        if goal.is_empty() {
                            "用法: :evo <目标描述>".to_string()
                        } else {
                            format!("进化任务已排队: {goal}（TUI 模式异步执行待完善）")
                        }
                    }
                    ":evo-status" => "进化状态查询（TUI 模式异步执行待完善）".to_string(),
                    ":evo-approve" => "确认合并（TUI 模式异步执行待完善）".to_string(),
                    ":evo-reject" => "拒绝回滚（TUI 模式异步执行待完善）".to_string(),
                    ":evo-diff" => "进化变更查询（TUI 模式异步执行待完善）".to_string(),
                    _ => unreachable!(),
                };
                self.output.push_system(&msg);
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

// ─── Markdown 渲染 ──────────────────────────────────────────────────

/// 将 markdown 文本渲染为带样式的终端行
fn render_markdown_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;

    for line in text.lines() {
        // ── 代码块边界 ──
        if line.trim_start().starts_with("```") {
            if in_code_block {
                in_code_block = false;
                lines.push(Line::from(Span::styled(
                    String::from("  └──"),
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                in_code_block = true;
                let lang = line.trim_start().trim_start_matches('`').trim();
                let label = if lang.is_empty() {
                    String::new()
                } else {
                    format!(" {lang}")
                };
                lines.push(Line::from(Span::styled(
                    format!("  ┌──{label}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            continue;
        }

        if in_code_block {
            lines.push(Line::from(Span::styled(
                format!("  │ {line}"),
                Style::default().fg(Color::Green),
            )));
            continue;
        }

        // ── 标题 ──
        if let Some(rest) = line.strip_prefix("### ") {
            lines.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            lines.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            lines.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }

        // ── 引用 ──
        if let Some(rest) = line.strip_prefix("> ") {
            lines.push(Line::from(Span::styled(
                format!("  │ {rest}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
            continue;
        }

        // ── 无序列表 ──
        if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let mut spans = vec![Span::styled(
                String::from("  • "),
                Style::default().fg(Color::Cyan),
            )];
            spans.extend(parse_inline_markdown(rest));
            lines.push(Line::from(spans));
            continue;
        }

        // ── 有序列表 ──
        if let Some(dot_pos) = line.find(". ") {
            let prefix = &line[..dot_pos];
            if prefix.chars().all(|c| c.is_ascii_digit()) && !prefix.is_empty() {
                let rest = &line[dot_pos + 2..];
                let mut spans = vec![Span::styled(
                    format!("  {prefix}. "),
                    Style::default().fg(Color::Cyan),
                )];
                spans.extend(parse_inline_markdown(rest));
                lines.push(Line::from(spans));
                continue;
            }
        }

        // ── 分割线 ──
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            lines.push(Line::from(Span::styled(
                String::from("  ──────────────"),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        // ── 普通行：解析行内 markdown ──
        lines.push(Line::from(parse_inline_markdown(line)));
    }

    lines
}

/// 解析行内 Markdown：**bold**、*italic*、`code`、[link](url)
fn parse_inline_markdown(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut chars = text.chars().peekable();
    let mut buf = String::new();

    while let Some(ch) = chars.next() {
        match ch {
            // ── **bold** ──
            '*' if chars.peek() == Some(&'*') => {
                chars.next(); // 消费第二个 *
                flush_buf(&mut spans, &mut buf);
                let bold = collect_until_marker(&mut chars, &['*', '*']);
                if let Some(content) = bold {
                    spans.push(Span::styled(
                        content,
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                }
            }
            // ── *italic* ──
            '*' => {
                flush_buf(&mut spans, &mut buf);
                let italic = collect_until_char(&mut chars, '*');
                if let Some(content) = italic {
                    spans.push(Span::styled(
                        content,
                        Style::default().add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            // ── `code` ──
            '`' => {
                flush_buf(&mut spans, &mut buf);
                let code = collect_until_char(&mut chars, '`');
                if let Some(content) = code {
                    spans.push(Span::styled(content, Style::default().fg(Color::Yellow)));
                }
            }
            // ── [link](url) ──
            '[' => {
                flush_buf(&mut spans, &mut buf);
                let (link_text, matched) = collect_link(&mut chars);
                if matched {
                    spans.push(Span::styled(
                        link_text,
                        Style::default()
                            .fg(Color::Blue)
                            .add_modifier(Modifier::UNDERLINED),
                    ));
                } else {
                    spans.push(Span::raw(format!("[{link_text}")));
                }
            }
            _ => buf.push(ch),
        }
    }

    flush_buf(&mut spans, &mut buf);
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

/// 将缓冲区内容刷入 spans
fn flush_buf(spans: &mut Vec<Span<'static>>, buf: &mut String) {
    if !buf.is_empty() {
        spans.push(Span::raw(std::mem::take(buf)));
    }
}

/// 收集字符直到遇到连续标记（如 **）
fn collect_until_marker(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    markers: &[char],
) -> Option<String> {
    let mut result = String::new();
    loop {
        match chars.next() {
            Some(ch) => {
                if ch == markers[0] && chars.peek() == Some(&markers[1]) {
                    chars.next(); // 消费第二个标记
                    return Some(result);
                }
                result.push(ch);
            }
            None => {
                // 未闭合，原样返回（带标记前缀）
                result = format!("{}{}", markers.iter().collect::<String>(), result);
                return Some(result);
            }
        }
    }
}

/// 收集字符直到遇到指定字符
fn collect_until_char(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    end: char,
) -> Option<String> {
    let mut result = String::new();
    loop {
        match chars.next() {
            Some(ch) if ch == end => return Some(result),
            Some(ch) => result.push(ch),
            None => {
                // 未闭合，原样返回（带起始标记）
                result = format!("{end}{result}");
                return Some(result);
            }
        }
    }
}

/// 收集链接文本和 URL：[text](url)
fn collect_link(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> (String, bool) {
    let mut text = String::new();
    // 收集 [text]
    loop {
        match chars.next() {
            Some(']') => break,
            Some(ch) => text.push(ch),
            None => return (text, false),
        }
    }
    // 检查是否紧跟 (
    if chars.peek() != Some(&'(') {
        return (text, false);
    }
    chars.next(); // 消费 (
                  // 跳过 URL（不存储）
    loop {
        match chars.next() {
            Some(')') => return (text, true),
            None => return (text, false),
            _ => {}
        }
    }
}
