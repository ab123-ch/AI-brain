//! TUI 主应用组件
//!
//! 修复清单:
//! - Spinner 不再累积到 lines 中，只保留一个独立状态
//! - 输出区域自动滚到底部，Shift+↑/↓ 滚动
//! - `e` 键只在无文本输入时生效（通过 Ctrl+E 触发展开）
//! - finish_query 不阻塞事件循环

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use brain_core::types::ProgressEvent;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;

use crate::command::{self, CommandHandler, CommandRegistry};
use crate::orchestrator::Orchestrator;
use brain_mcp::client_pool::ServerStatus;

use super::command_panel::CommandPanel;
use super::completion::EvolutionCompleter;
use super::input::{InputArea, InputResult};
use super::output::{OutputArea, OutputLine};
use super::status::StatusBar;

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⼾", "⼿", "⾀", "⾁", "⾂", "⾃"];
const COLLAPSE_MAX_CHARS: usize = 80;

/// AskUserQuestion 的选择状态（底部内联面板）
struct SelectionState {
    /// 问题文本
    question: String,
    /// 选项列表
    options: Vec<String>,
    /// 光标位置（当前高亮的索引）
    cursor_index: usize,
    /// 多选模式下已选中的索引集合
    selected_indices: HashSet<usize>,
    /// 是否允许多选
    multi_select: bool,
}

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
    /// 协作取消令牌：调用 cancel() 让 tool_loop 优雅退出，已执行的工具调用结果不丢失
    cancel_token: Option<tokio_util::sync::CancellationToken>,
    /// Done 事件已收到，等待 query_handle 完成
    done_received: bool,
    /// 待发送消息队列（busy 时 Enter 提交的消息排队等处理）
    pending_queue: Vec<String>,
    /// 上一次键盘事件时间
    last_event_time: Instant,
    /// 等待用户回答 AskUserQuestion 的 sender
    pending_ask_response: Option<tokio::sync::oneshot::Sender<String>>,
    /// AskUserQuestion 选择弹框状态（有选项时激活）
    selection_state: Option<SelectionState>,
    // --- 选择系统 ---
    /// 输出区域位置（render 时缓存）
    output_rect: Rect,
    /// 当前滚动偏移（render 时缓存）
    current_scroll: u16,
    /// 鼠标点击的原始终端坐标 (row, col)
    cursor_pos: Option<(u16, u16)>,
    /// 选择起点 (rendered_text 行号, 显示列号)
    selection_anchor: Option<(usize, usize)>,
    /// 选择终点 (rendered_text 行号, 显示列号)
    selection_end: Option<(usize, usize)>,
    /// 扩展模式：点击时设终点而非重设锚点
    selection_extend_mode: bool,
    /// 渲染后的纯文本行（用于复制）
    rendered_text: Vec<String>,
    /// 命令注册表（所有命令的中央仓库）
    command_registry: CommandRegistry,
    /// 命令面板（下拉选择 UI）
    command_panel: CommandPanel,
}

impl App {
    pub fn new(orch: Orchestrator) -> Self {
        let orch = Arc::new(orch);
        let status_structured = orch.status_structured();
        let mut output = OutputArea::new();
        output.push_system("AI Brain v2 一主二从系统");
        output.push_system(
            "输入查询 | :help 命令 | Enter 提交 | Tab 补全 | ↑↓ 历史 | Shift+Enter 换行",
        );
        output.push_system("文本选择: 单击拖选 | 单击→Ctrl+F→单击扩展 → Ctrl+Y 复制 | Esc 清除");

        // 构建 CommandRegistry，同时用于补全器和命令面板
        let command_registry = command::build_full_registry();

        // 从 orchestrator 获取 brain-evolution 数据，从 registry 动态读取命令列表
        let completer = {
            let (template_names, pattern_keywords) = orch.completion_data();
            EvolutionCompleter::from_registry(&command_registry, template_names, pattern_keywords)
        };

        let command_panel = CommandPanel::new();

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
            cancel_token: None,
            done_received: false,
            pending_queue: Vec::new(),
            last_event_time: Instant::now(),
            pending_ask_response: None,
            selection_state: None,
            output_rect: Rect::default(),
            current_scroll: 0,
            cursor_pos: None,
            selection_anchor: None,
            selection_end: None,
            selection_extend_mode: false,
            rendered_text: Vec::new(),
            command_registry,
            command_panel,
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

        // 选择面板激活时，隐藏输入区，显示提示
        if let Some(ref sel) = self.selection_state {
            // 在输入区位置渲染提示
            let hint_text = if sel.multi_select {
                "  ↑↓ 移动 · 空格选择/取消 · Enter 确认 · Esc 取消 · 直接输入自定义回答"
            } else {
                "  ↑↓ 选择 · Enter 确认 · Esc 取消 · 直接输入自定义回答"
            };
            let hint = Paragraph::new(Line::from(Span::styled(
                hint_text,
                Style::default().fg(Color::Cyan),
            )));
            f.render_widget(hint, chunks[2]);
        } else {
            f.render_widget(&self.input.textarea, chunks[2]);
        }

        // 渲染选择面板（输出区域底部的内联面板，非浮动弹框）
        self.render_selection_block(f, chunks[0]);

        // 渲染命令面板（浮动在输入区上方）
        self.command_panel.render(f, chunks[2]);
    }

    fn render_output(&mut self, f: &mut ratatui::Frame, area: Rect) {
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
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        }

        // Spinner 行（独立渲染，不在 lines 中）
        if let Some(spinner) = self.output.spinner_text() {
            ratatui_lines.push(Line::from(Span::styled(
                format!("  {spinner}"),
                Style::default().fg(Color::Blue),
            )));
        }

        // 预换行：将逻辑行拆成屏幕行（每个 Line 恰好占一个屏幕行）
        // 渲染和选择共用同一份换行结果，消除坐标映射偏差
        let area_width = area.width as usize;
        let mut final_lines: Vec<Line<'static>> = Vec::new();
        self.rendered_text.clear();

        for line in &ratatui_lines {
            let full_text: String = line.iter().map(|span| span.content.as_ref()).collect();
            let segments = Self::wrap_text_to_width(&full_text, area_width);

            if segments.len() <= 1 {
                // 不需要换行，保持原始样式
                final_lines.push(line.clone());
                self.rendered_text.extend(segments);
            } else {
                // 需要换行：检测前缀样式（如 "> "、"✓ "）
                let content_style = line.iter().last().map(|s| s.style).unwrap_or_default();
                let (prefix_style, prefix_end_col) = if let Some(first) = line.iter().next() {
                    let w: usize = first
                        .content
                        .chars()
                        .map(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1))
                        .sum();
                    if w > 0 && w <= 4 {
                        (first.style, w)
                    } else {
                        (Style::default(), 0)
                    }
                } else {
                    (Style::default(), 0)
                };

                for (i, seg) in segments.iter().enumerate() {
                    if i == 0 && prefix_end_col > 0 {
                        // 首段：在前缀边界拆分，保持前缀+内容双样式
                        let mut col = 0usize;
                        let mut split = 0usize;
                        for (j, ch) in seg.chars().enumerate() {
                            if col >= prefix_end_col {
                                split = j;
                                break;
                            }
                            col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                        }
                        let pre: String = seg.chars().take(split).collect();
                        let rest: String = seg.chars().skip(split).collect();
                        final_lines.push(Line::from(vec![
                            Span::styled(pre, prefix_style),
                            Span::styled(rest, content_style),
                        ]));
                    } else {
                        // 续行或无前缀行：使用内容样式
                        final_lines.push(Line::from(Span::styled(seg.clone(), content_style)));
                    }
                    self.rendered_text.push(seg.clone());
                }
            }
        }

        // 滚动计算：rendered_text 已经是屏幕行，直接计数
        let visible_lines = area.height as usize;
        let max_scroll = self.rendered_text.len().saturating_sub(visible_lines) as u16;

        // resize 后 clamp manual_scroll，防止溢出
        self.output.clamp_scroll(max_scroll);

        let scroll = if self.output.manual_scroll > 0 {
            max_scroll.saturating_sub(self.output.manual_scroll)
        } else {
            max_scroll
        };

        // 缓存供鼠标事件使用
        self.output_rect = area;
        self.current_scroll = scroll;

        // 用预换行后的 Lines 渲染，不用 .wrap()，保证坐标一一对应
        let paragraph = Paragraph::new(final_lines)
            .block(Block::default().borders(Borders::NONE))
            .scroll((scroll, 0));

        f.render_widget(paragraph, area);

        // 光标和高亮标记
        {
            let buf = f.buffer_mut();

            // 单点光标（仅无选择时显示）
            if self.selection_anchor.is_none() && self.selection_end.is_none() {
                if let Some((row, col)) = self.cursor_pos {
                    if let Some(cell) = buf.cell_mut((col, row)) {
                        cell.set_bg(Color::Yellow);
                        cell.set_fg(Color::Black);
                    }
                }
            }

            // 选择起点标记（仅 anchor 无 end 时显示黄点）
            if let Some((rt_row, dcol)) = self.selection_anchor {
                if self.selection_end.is_none() {
                    let buf_row = self.output_rect.top()
                        + (rt_row as u16).saturating_sub(self.current_scroll);
                    let buf_col = self.output_rect.left() + dcol as u16;
                    if let Some(cell) = buf.cell_mut((buf_col, buf_row)) {
                        cell.set_bg(Color::Yellow);
                        cell.set_fg(Color::Black);
                    }
                }
            }

            // 选择区域高亮（anchor + end 都有值时显示青色背景）
            if self.selection_anchor.is_some() && self.selection_end.is_some() {
                self.render_selection_highlight(buf);
            }
        }
    }

    /// 渲染 AskUserQuestion 选择面板（输出区域底部的内联面板，非浮动弹框）
    fn render_selection_block(&self, f: &mut ratatui::Frame, area: Rect) {
        let sel = match self.selection_state {
            Some(ref s) => s,
            None => return,
        };

        // 面板高度：标题(1) + 选项(N) + 提示(1) + 边框(2)
        let inner_height = sel.options.len() as u16 + 4;
        // 面板锚定在输出区域底部
        let block_height = inner_height.min(area.height);
        let block_area = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(block_height),
            width: area.width,
            height: block_height,
        };

        // 清除面板区域底层内容
        f.render_widget(ratatui::widgets::Clear, block_area);

        // 边框（问题作为标题）
        let block = ratatui::widgets::Block::default()
            .title(ratatui::text::Span::styled(
                format!(" {} ", &sel.question),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(ratatui::layout::Alignment::Left)
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        // 构建选项列表
        let items: Vec<ratatui::widgets::ListItem> = sel
            .options
            .iter()
            .enumerate()
            .map(|(i, opt)| {
                let is_cursor = i == sel.cursor_index;
                let is_selected = sel.selected_indices.contains(&i);
                // 样式：光标行高亮，已选项标记
                let style = if is_cursor {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else if is_selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                // 前缀：光标用 ▶，已选用 ✓，多选框用 ◻/◼
                let prefix = if sel.multi_select {
                    let check = if is_selected { "◼" } else { "◻" };
                    if is_cursor {
                        format!("▶ {check} ")
                    } else {
                        format!("  {check} ")
                    }
                } else if is_cursor {
                    "▶ ".to_string()
                } else {
                    "  ".to_string()
                };
                ratatui::widgets::ListItem::new(Line::from(Span::styled(
                    format!("{prefix}{opt}"),
                    style,
                )))
            })
            .collect();

        let list = ratatui::widgets::List::new(items).block(block);
        f.render_widget(list, block_area);
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
                            if let Some(last) =
                                all_lines.iter().rev().find(|l| !l.trim().is_empty())
                            {
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
        // 过滤掉 Release 事件（Windows 上输入法会产生 Release 事件导致重复输入）
        if key.kind == KeyEventKind::Release {
            return false;
        }

        let now = Instant::now();
        self.last_event_time = now;

        // ── 选择面板优先拦截 ──
        if self.selection_state.is_some() {
            match (key.modifiers, key.code) {
                // 方向键上：移动光标到前一项
                (KeyModifiers::NONE, KeyCode::Up) => {
                    if let Some(ref mut sel) = self.selection_state {
                        sel.cursor_index = sel.cursor_index.saturating_sub(1);
                    }
                    return true;
                }
                // 方向键下：移动光标到后一项
                (KeyModifiers::NONE, KeyCode::Down) => {
                    if let Some(ref mut sel) = self.selection_state {
                        sel.cursor_index =
                            (sel.cursor_index + 1).min(sel.options.len().saturating_sub(1));
                    }
                    return true;
                }
                // 空格：多选模式下切换选中/取消；单选模式下等同 Enter
                (KeyModifiers::NONE, KeyCode::Char(' ')) => {
                    if let Some(ref mut sel) = self.selection_state {
                        if sel.multi_select {
                            if sel.selected_indices.contains(&sel.cursor_index) {
                                sel.selected_indices.remove(&sel.cursor_index);
                            } else {
                                sel.selected_indices.insert(sel.cursor_index);
                            }
                        } else {
                            // 单选模式下，空格等同 Enter
                            let sel_state = self.selection_state.take().unwrap();
                            let answer = sel_state.options[sel_state.cursor_index].clone();
                            self.output.push_system(&format!("   ✓ 已选择: {answer}"));
                            if let Some(tx) = self.pending_ask_response.take() {
                                let _ = tx.send(answer);
                            }
                        }
                    }
                    return true;
                }
                // Enter：确认选择
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    if let Some(sel) = self.selection_state.take() {
                        if sel.multi_select {
                            // 多选模式：如果没选任何项，自动选中光标所在项
                            let indices = if sel.selected_indices.is_empty() {
                                std::iter::once(sel.cursor_index).collect()
                            } else {
                                sel.selected_indices
                            };
                            let answers: Vec<String> = indices
                                .iter()
                                .filter_map(|&i| sel.options.get(i).cloned())
                                .collect();
                            let answer = answers.join(", ");
                            self.output.push_system(&format!("   ✓ 已选择: {answer}"));
                            if let Some(tx) = self.pending_ask_response.take() {
                                let _ = tx.send(answer);
                            }
                        } else {
                            let answer = sel.options[sel.cursor_index].clone();
                            self.output.push_system(&format!("   ✓ 已选择: {answer}"));
                            if let Some(tx) = self.pending_ask_response.take() {
                                let _ = tx.send(answer);
                            }
                        }
                    }
                    return true;
                }
                // Esc：取消选择面板，降级为自由输入
                (_, KeyCode::Esc) => {
                    self.selection_state = None;
                    self.output.push_system("   选择已取消，请直接输入回答:");
                    return true;
                }
                // 其他键：退出选择模式，让输入框接收按键（自定义回答）
                _ => {
                    self.selection_state = None;
                    self.output.push_system("   请直接输入自定义回答:");
                    // 不 return，让按键继续传递到 input 层
                }
            }
        }

        // ── 命令面板导航（可见时优先拦截方向键/Enter/Tab/Esc）──
        if self.command_panel.visible {
            match key.code {
                KeyCode::Down => {
                    self.command_panel.move_down();
                    return true;
                }
                KeyCode::Up => {
                    self.command_panel.move_up();
                    return true;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    if let Some(item) = self.command_panel.confirm() {
                        let mut replacement = item.display.clone();
                        let has_subcommands = self
                            .command_registry
                            .find_command(&item.command_name)
                            .map_or(false, |c| !c.subcommands.is_empty());
                        if has_subcommands && item.subcommand_name.is_none() {
                            replacement.push(' ');
                        } else if item.subcommand_name.is_some() {
                            replacement.push(' ');
                        }
                        self.input.set_text(replacement);
                        // 重新触发面板过滤
                        self.command_panel
                            .update_filter(&self.command_registry, &self.input.input_text());
                    }
                    return true;
                }
                KeyCode::Esc => {
                    self.command_panel.clear();
                    return true;
                }
                _ => {} // 其他按键穿透到正常处理
            }
        }

        // ── App 层独占按键 ──
        match (key.modifiers, key.code) {
            // Ctrl+C / Esc — 清除选择、取消查询或退出
            (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => {
                // 清除选择、光标和扩展模式
                if self.selection_anchor.is_some()
                    || self.cursor_pos.is_some()
                    || self.selection_extend_mode
                {
                    self.selection_anchor = None;
                    self.selection_end = None;
                    self.cursor_pos = None;
                    self.selection_extend_mode = false;
                    return true;
                }
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
            // Ctrl+F — 切换选择扩展模式
            (KeyModifiers::CONTROL, KeyCode::Char('f')) => {
                if self.selection_anchor.is_some() && self.selection_end.is_none() {
                    self.selection_extend_mode = !self.selection_extend_mode;
                    if self.selection_extend_mode {
                        self.output
                            .push_system("  选择扩展模式: 点击或拖拽设终点 (Esc取消)");
                    }
                } else {
                    self.selection_extend_mode = false;
                }
                return true;
            }
            // Ctrl+Y — 复制选中内容到剪贴板
            (KeyModifiers::CONTROL, KeyCode::Char('y')) => {
                self.copy_selection();
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
            InputResult::Consumed => {
                // 输入变化时更新命令面板
                let text = self.input.input_text();
                if text.starts_with(':') {
                    self.command_panel
                        .update_filter(&self.command_registry, &text);
                } else if self.command_panel.visible {
                    self.command_panel.clear();
                }
            }
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

        // 重置命令面板状态，避免 phase 残留导致下次输入 : 无法弹出下拉
        self.command_panel.clear();

        // 如果有等待中的 AskUserQuestion，直接发送响应
        if let Some(tx) = self.pending_ask_response.take() {
            self.output.push_user_input(&text);
            let _ = tx.send(text);
            return;
        }

        // 内置命令始终立即处理
        match self.handle_builtin_command_sync(&text) {
            HandleResult::Handled => return,
            HandleResult::Exit => {
                self.should_quit = true;
                return;
            }
            HandleResult::Unknown => {}
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
        let (rx, handle, cancel) = Arc::clone(&self.orch).query_streaming(text);
        self.progress_rx = Some(rx);
        self.query_handle = Some(handle);
        self.cancel_token = Some(cancel);
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
                        // AskUser 事件：提取 sender 存储，显示问题和选项
                        if let ProgressEvent::AskUser {
                            question,
                            options,
                            multi_select,
                            response_tx,
                        } = event
                        {
                            self.pending_ask_response = Some(response_tx.0);
                            // 显示 ToolStart 行（和 handle_event 中 ToolStart 一样）
                            self.output.handle_event(&ProgressEvent::ToolStart {
                                brain: "main".into(),
                                tool_name: "AskUserQuestion".into(),
                                input: serde_json::to_string(&serde_json::json!({
                                    "question": &question,
                                    "options": &options,
                                    "multi_select": multi_select,
                                }))
                                .unwrap_or_default(),
                            });
                            // 显示问题文本
                            self.output.push_system(&format!("❓ {question}"));

                            if let Some(opts) = options {
                                if !opts.is_empty() {
                                    // 有选项时激活选择面板
                                    self.selection_state = Some(SelectionState {
                                        question,
                                        options: opts,
                                        cursor_index: 0,
                                        selected_indices: HashSet::new(),
                                        multi_select,
                                    });
                                } else {
                                    // 空选项列表，等用户自由输入
                                    self.output.push_system("   请输入回答:");
                                }
                            } else {
                                // 无选项时只显示问题，等用户自由输入
                                self.output.push_system("   请输入回答:");
                            }
                            continue;
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

    /// 复制选中内容到系统剪贴板（macOS: pbcopy）
    fn copy_selection(&mut self) {
        if let (Some(anchor), Some(end)) = (self.selection_anchor, self.selection_end) {
            let ((start_line, start_col), (end_line, end_col)) =
                if anchor.0 < end.0 || (anchor.0 == end.0 && anchor.1 <= end.1) {
                    (anchor, end)
                } else {
                    (end, anchor)
                };

            // 字符级别提取选中文本
            let mut parts: Vec<String> = Vec::new();
            for (i, line_text) in self.rendered_text.iter().enumerate() {
                if i < start_line || i > end_line {
                    continue;
                }

                if start_line == end_line {
                    // 单行选择：提取 start_col..=end_col
                    let ci_s = Self::screen_col_to_char_idx(line_text, start_col);
                    let ci_e = Self::screen_col_to_char_idx(line_text, end_col + 1);
                    let selected: String = line_text
                        .chars()
                        .skip(ci_s)
                        .take(ci_e.saturating_sub(ci_s))
                        .collect();
                    parts.push(selected);
                } else if i == start_line {
                    // 首行：从 start_col 到行尾
                    let ci = Self::screen_col_to_char_idx(line_text, start_col);
                    let selected: String = line_text.chars().skip(ci).collect();
                    parts.push(selected);
                } else if i == end_line {
                    // 尾行：从行首到 end_col
                    let ci = Self::screen_col_to_char_idx(line_text, end_col + 1);
                    let selected: String = line_text.chars().take(ci).collect();
                    parts.push(selected);
                } else {
                    // 中间行：整行
                    parts.push(line_text.clone());
                }
            }

            let text = parts.join("\n");

            let text_preview: String = text.chars().take(60).collect();
            let start_preview: String = self
                .rendered_text
                .get(start_line)
                .map(|s| s.chars().take(30).collect())
                .unwrap_or_default();
            let end_preview: String = self
                .rendered_text
                .get(end_line)
                .map(|s| s.chars().take(30).collect())
                .unwrap_or_default();
            Self::dbg_log(&format!(
                "COPY: anchor={:?} end={:?} -> start=({},{}) end=({},{}) | text={:?} | line[{}]={:?} | line[{}]={:?}",
                self.selection_anchor, self.selection_end,
                start_line, start_col, end_line, end_col,
                text_preview, start_line, start_preview, end_line, end_preview
            ));

            if text.is_empty() {
                self.output.push_system("选择范围为空");
                return;
            }

            // 用 pbcopy 复制到剪贴板
            match std::process::Command::new("pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                Ok(mut child) => {
                    if let Some(mut stdin) = child.stdin.take() {
                        use std::io::Write as _;
                        let _ = stdin.write_all(text.as_bytes());
                    }
                    let _ = child.wait();
                    let char_count = text.chars().count();
                    self.output
                        .push_system(&format!("已复制 {char_count} 个字符到剪贴板"));
                }
                Err(_) => {
                    self.output.push_system("复制失败: pbcopy 不可用");
                }
            }

            // 复制后清除选择
            self.selection_anchor = None;
            self.selection_end = None;
        } else {
            self.output
                .push_system("未选择文本（单击拖选 / 单击后右键扩展）");
        }
    }

    fn cancel_query(&mut self) {
        // 协作取消：通知 tool_loop 停止（而非直接 abort）
        // tool_loop 检测到取消信号后会返回已执行的部分结果，
        // process_input 会将已执行的工具调用写入历史，避免信息丢失。
        if let Some(cancel) = self.cancel_token.take() {
            cancel.cancel();
            tracing::info!("已发送协作取消信号，tool_loop 将在下一轮 LLM 调用前退出");
        }
        // 不 abort task — 让它自然完成，process_input 内部会写回已执行结果
        // JoinHandle drop 时自动 detach
        self.query_handle = None;
        self.progress_rx = None;
        self.done_received = false;
        self.is_busy = false;
        self.status.busy = false;
        self.pending_queue.clear();
        self.output
            .push_system("查询已取消（已执行的工具调用已保存）");
    }

    fn handle_ctrl_c(&mut self) {
        if self.is_busy {
            self.cancel_query();
        } else {
            self.should_quit = true;
        }
    }

    /// 处理鼠标事件
    ///
    /// - 滚轮: 上下滚动
    /// - 第一次单击: 设置选择起点
    /// - 第二次单击: 设置选择终点，高亮两点之间的字符
    /// 将屏幕列号转换为字符索引（考虑 CJK 双宽字符）
    fn screen_col_to_char_idx(text: &str, col: usize) -> usize {
        let mut current_col = 0;
        for (i, ch) in text.chars().enumerate() {
            if current_col >= col {
                return i;
            }
            current_col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        }
        text.chars().count()
    }

    /// 按显示宽度将文本换行，返回每个屏幕行的文本片段
    fn wrap_text_to_width(text: &str, max_width: usize) -> Vec<String> {
        if max_width == 0 {
            return vec![text.to_string()];
        }
        let mut result = Vec::new();
        let mut current = String::new();
        let mut current_width = 0;

        for ch in text.chars() {
            let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
            if current_width + ch_width > max_width && !current.is_empty() {
                result.push(std::mem::take(&mut current));
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }
        if !current.is_empty() {
            result.push(current);
        }
        if result.is_empty() {
            result.push(String::new());
        }
        result
    }

    /// 写调试日志到 /tmp/tui_sel.log
    fn dbg_log(msg: &str) {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/tui_sel.log")
        {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let _ = writeln!(f, "[{ts}] {msg}");
        }
    }

    /// 将终端坐标转换为 rendered_text 行号和显示列号
    fn terminal_to_rendered_coords(&self, term_row: u16, term_col: u16) -> (usize, u16) {
        let scroll = self.current_scroll as usize;
        let top = self.output_rect.top() as usize;
        let left = self.output_rect.left() as usize;

        // rendered_text 行号 = scroll + (term_row - output_area_top)
        let rt_row = scroll + term_row.saturating_sub(top as u16) as usize;

        // 终端列号减去输出区域左偏移后，视为 char offset，再转 display_col
        let char_offset = term_col.saturating_sub(left as u16) as usize;
        let display_col = if rt_row < self.rendered_text.len() {
            let line = &self.rendered_text[rt_row];
            let mut dcol: usize = 0;
            for (i, ch) in line.chars().enumerate() {
                if i >= char_offset {
                    break;
                }
                dcol += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
            }
            dcol as u16
        } else {
            0
        };

        (rt_row, display_col)
    }

    /// 渲染文本选择高亮到 buffer
    fn render_selection_highlight(&self, buf: &mut ratatui::buffer::Buffer) {
        let (anchor, end) = match (self.selection_anchor, self.selection_end) {
            (Some(a), Some(e)) => (a, e),
            _ => return,
        };

        // 排序起点/终点
        let ((start_line, start_col), (end_line, end_col)) =
            if anchor.0 < end.0 || (anchor.0 == end.0 && anchor.1 <= end.1) {
                (anchor, end)
            } else {
                (end, anchor)
            };

        let scroll = self.current_scroll as usize;
        let top = self.output_rect.top() as usize;
        let left = self.output_rect.left() as usize;
        let height = self.output_rect.height as usize;
        let width = self.output_rect.width as usize;

        // 可见行范围
        let visible_start = scroll;
        let visible_end = scroll + height;

        // 限制到可见范围
        let sel_start = start_line.max(visible_start);
        let sel_end = end_line.min(visible_end.saturating_sub(1));

        if sel_start > sel_end {
            return;
        }

        for rt_row in sel_start..=sel_end {
            let buf_row = top + (rt_row - scroll);
            if buf_row >= top + height {
                break;
            }

            // 计算该行文本的显示宽度
            let line_width: usize = if rt_row < self.rendered_text.len() {
                self.rendered_text[rt_row]
                    .chars()
                    .map(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1))
                    .sum()
            } else {
                0
            };

            // 计算该行的列范围
            let (col_start, col_end) = if rt_row == start_line && rt_row == end_line {
                // 单行选择：需要包含 end_col 处字符的完整宽度
                let ci = Self::screen_col_to_char_idx(&self.rendered_text[rt_row], end_col);
                let ch_w = self.rendered_text[rt_row]
                    .chars()
                    .nth(ci)
                    .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1))
                    .unwrap_or(1);
                (start_col, (end_col + ch_w).min(width))
            } else if rt_row == start_line {
                // 首行：从 start_col 到行尾
                (start_col, line_width)
            } else if rt_row == end_line {
                // 尾行：从行首到 end_col（含该字符完整宽度）
                let ci = Self::screen_col_to_char_idx(&self.rendered_text[rt_row], end_col);
                let ch_w = self.rendered_text[rt_row]
                    .chars()
                    .nth(ci)
                    .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1))
                    .unwrap_or(1);
                (0, (end_col + ch_w).min(width))
            } else {
                // 中间行：整行
                (0, line_width)
            };

            // 高亮每个 cell
            for col in col_start..col_end {
                let buf_col = left + col;
                if let Some(cell) = buf.cell_mut((buf_col as u16, buf_row as u16)) {
                    cell.set_bg(Color::Cyan);
                    cell.set_fg(Color::Black);
                }
            }
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        // 判断是否在输出区域
        let in_output = mouse.row >= self.output_rect.top()
            && mouse.row < self.output_rect.top() + self.output_rect.height;

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.output.scroll_up(3);
            }
            MouseEventKind::ScrollDown => {
                self.output.scroll_down(3);
            }
            MouseEventKind::Down(event::MouseButton::Left) if in_output => {
                let (rt_row, display_col) =
                    self.terminal_to_rendered_coords(mouse.row, mouse.column);

                if rt_row >= self.rendered_text.len() {
                    return;
                }

                if self.selection_extend_mode && self.selection_anchor.is_some() {
                    // 扩展模式: 点击设终点
                    self.selection_end = Some((rt_row, display_col as usize));
                    self.selection_extend_mode = false; // 设完终点自动退出
                } else {
                    // 普通点击: 重设锚点
                    self.selection_anchor = Some((rt_row, display_col as usize));
                    self.selection_end = None;
                    self.selection_extend_mode = false;
                }
                self.cursor_pos = None;
            }
            // 右键点击: 扩展选择（macOS 双指点击 / Ctrl+Click 都会触发）
            MouseEventKind::Down(event::MouseButton::Right) if in_output => {
                if self.selection_anchor.is_some() {
                    let (rt_row, display_col) =
                        self.terminal_to_rendered_coords(mouse.row, mouse.column);
                    if rt_row < self.rendered_text.len() {
                        self.selection_end = Some((rt_row, display_col as usize));
                        self.cursor_pos = None;
                    }
                }
            }
            MouseEventKind::Drag(event::MouseButton::Left) if in_output => {
                if self.selection_anchor.is_some() {
                    let (rt_row, display_col) =
                        self.terminal_to_rendered_coords(mouse.row, mouse.column);
                    if rt_row < self.rendered_text.len() {
                        self.selection_end = Some((rt_row, display_col as usize));
                    }
                }
            }
            _ => {}
        }
    }

    /// 处理内置命令（通过 CommandRegistry 查找并执行）
    fn handle_builtin_command_sync(&mut self, input: &str) -> HandleResult {
        let trimmed = input.trim();

        // 快速排除非命令输入
        if !trimmed.starts_with(':') && trimmed != "help" && trimmed != "exit" && trimmed != "quit"
        {
            return HandleResult::Unknown;
        }

        // 去掉冒号前缀，"help"/"exit"/"quit" 无冒号也支持
        let cmd_input = if trimmed.starts_with(':') {
            trimmed.trim_start_matches(':')
        } else {
            trimmed
        };
        let parts: Vec<&str> = cmd_input.split_whitespace().collect();
        if parts.is_empty() {
            return HandleResult::Unknown;
        }

        let cmd_name = parts[0];
        let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

        // 特殊: status 命令需要 Orchestrator 实时状态
        if cmd_name == "status" {
            self.output.push_system(&self.orch.status());
            return HandleResult::Handled;
        }

        // 特殊: skill list 需要从 Orchestrator 获取 SkillCatalog
        if cmd_name == "skill" && args.first().map(|s| s.as_str()) == Some("list") {
            let catalog = self.orch.skill_catalog();
            if catalog.skills.is_empty() {
                self.output.push_system("暂无已注册技能");
            } else {
                self.output.push_system(&format!("已注册技能 ({}个):", catalog.skills.len()));
                self.output.push_system("\x1b[33m  \x1b[0m\x1b[33m[兼容 Claude Code]\x1b[0m 也扫描 ~/.claude/skills 目录");
                for skill in &catalog.skills {
                    let display = match &skill.namespace {
                        Some(ns) => format!("{}:{}", ns, skill.name),
                        None => skill.name.clone(),
                    };
                    // 判断来源
                    let source_tag = if skill.source_path.to_string_lossy().contains(".claude/skills") {
                        "\x1b[33m[Claude]\x1b[0m "
                    } else if skill.source_path.to_string_lossy().contains(".codex/skills") {
                        "\x1b[33m[Codex]\x1b[0m "
                    } else {
                        ""
                    };
                    let when = skill
                        .when_to_use
                        .as_deref()
                        .map(|w| format!(" [触发: {w}]"))
                        .unwrap_or_default();
                    self.output
                        .push_system(&format!("  {}{} - {}{}", source_tag, display, skill.description, when));
                }
            }
            return HandleResult::Handled;
        }

        // 特殊: skill info 需要从 SkillCatalog 查询详情
        if cmd_name == "skill" && args.first().map(|s| s.as_str()) == Some("info") {
            if let Some(name) = args.get(1) {
                let catalog = self.orch.skill_catalog();
                match catalog.resolve(name) {
                    Some(meta) => {
                        let display = match &meta.namespace {
                            Some(ns) => format!("{}:{}", ns, meta.name),
                            None => meta.name.clone(),
                        };
                        self.output.push_system(&format!("技能: {display}"));
                        self.output.push_system(&format!("  描述: {}", meta.description));
                        if let Some(ref when) = meta.when_to_use {
                            self.output.push_system(&format!("  触发条件: {when}"));
                        }
                        self.output
                            .push_system(&format!("  来源: {}", meta.source_path.display()));
                        match catalog.load_content(meta) {
                            Ok(body) => {
                                self.output.push_system("  ─── 内容 ───");
                                for line in body.lines() {
                                    self.output.push_system(line);
                                }
                            }
                            Err(e) => self.output.push_system(&format!("  (读取内容失败: {e})")),
                        }
                    }
                    None => self.output.push_system(&format!("未找到技能: {name}")),
                }
            } else {
                self.output.push_system("用法: :skill info <name>");
            }
            return HandleResult::Handled;
        }

        // 特殊: plugin 命令需要 PluginManager
        if cmd_name == "plugin" {
            return self.handle_plugin_command(&args);
        }

        // 特殊: persona 命令需要记忆脑的 PersonaManager
        if cmd_name == "persona" {
            return self.handle_persona_command(&args);
        }

        // 特殊: memory 命令需要记忆脑
        if cmd_name == "memory" {
            return self.handle_memory_command(&args);
        }

        // 特殊: evo 命令需要 Orchestrator 异步方法
        if cmd_name == "evo" {
            return self.handle_evo_command(&args);
        }

        // 特殊: mcp list 需要从 McpClientPool 获取数据
        if cmd_name == "mcp" {
            return self.handle_mcp_command(&args);
        }

        // 特殊: exit/quit 无冒号前缀也支持
        if cmd_name == "exit" || cmd_name == "quit" {
            return HandleResult::Exit;
        }

        // 在注册表中查找命令
        let Some(cmd) = self.command_registry.find_command(cmd_name) else {
            return HandleResult::Unknown;
        };

        // 执行处理器
        match &cmd.handler {
            CommandHandler::Sync(f) => {
                let result = f(&args);
                if result.output == "__QUIT__" {
                    return HandleResult::Exit;
                }
                if result.output == "__CLEAR__" {
                    self.output.lines.clear();
                    return HandleResult::Handled;
                }
                for line in result.output.lines() {
                    self.output.push_system(line);
                }
                HandleResult::Handled
            }
            CommandHandler::Async(_) => {
                // 异步命令：显示占位提示
                self.output
                    .push_system(&format!("命令 :{} 异步执行中...（待完善）", cmd_name));
                HandleResult::Handled
            }
        }
    }
}

// ─── 命令拦截实现 ──────────────────────────────────────────────────

impl App {
    /// 人格管理命令拦截
    fn handle_persona_command(&mut self, args: &[String]) -> HandleResult {
        let sub = match args.first() {
            Some(s) => s.as_str(),
            None => {
                self.output.push_system("用法: :persona <list|switch|info|delete> [参数]");
                return HandleResult::Handled;
            }
        };

        let mem_arc = self.orch.memory_brain();
        let mut mem = match mem_arc.try_lock() {
            Ok(m) => m,
            Err(_) => {
                self.output.push_system("记忆脑正忙，请稍后重试");
                return HandleResult::Handled;
            }
        };

        match sub {
            "list" => {
                let personas = mem.persona_manager().list();
                let active_id = mem.persona_manager().active_id().to_string();
                self.output.push_system(&format!("已注册人格 ({}个):", personas.len()));
                for p in personas {
                    let marker = if p.id == active_id {
                        "\x1b[33m ← 当前\x1b[0m"
                    } else {
                        ""
                    };
                    self.output.push_system(&format!(
                        "  \x1b[36m{}\x1b[0m ({}) — {}{}",
                        p.name, p.id, p.description, marker
                    ));
                }
            }
            "switch" => {
                let id = match args.get(1) {
                    Some(id) => id.clone(),
                    None => {
                        self.output.push_system("用法: :persona switch <id>");
                        return HandleResult::Handled;
                    }
                };
                let new_name = mem.persona_manager().list()
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name.clone());
                match mem.switch_persona(&id) {
                    Ok(()) => {
                        let name = new_name.unwrap_or_else(|| id.clone());
                        drop(mem);
                        self.output.push_system(&format!(
                            "\x1b[32m已切换到人格: {}\x1b[0m",
                            name
                        ));
                        self.output.push_system("  (主脑上下文将在下次对话时刷新)");
                    }
                    Err(e) => {
                        self.output.push_system(&format!("切换失败: {e}"));
                    }
                }
            }
            "info" => {
                let p = mem.active_persona();
                self.output.push_system(&format!("\x1b[36m当前人格: {}\x1b[0m ({})", p.name, p.id));
                self.output.push_system(&format!("  描述: {}", p.description));
                self.output.push_system(&format!(
                    "  语言: {} | 风格: {} | 评估敏感度: {}",
                    p.config.language, p.config.output_style, p.config.eval_sensitivity
                ));
                self.output.push_system(&format!(
                    "  分析间隔: {} 轮",
                    p.config.analysis_interval
                ));
                if !p.system_prompt.is_empty() {
                    self.output.push_system(&format!("  Prompt: {}...", &p.system_prompt[..p.system_prompt.len().min(80)]));
                }
                let stats = mem.stats();
                if let Ok(s) = stats {
                    self.output.push_system(&format!(
                        "  记忆: L1={} L2={} L3={} L4={}",
                        s.l1_count, s.l2_count, s.l3_count, s.l4_exists
                    ));
                }
            }
            "delete" => {
                let id = match args.get(1) {
                    Some(id) => id.clone(),
                    None => {
                        self.output.push_system("用法: :persona delete <id>");
                        return HandleResult::Handled;
                    }
                };
                match mem.persona_manager_mut().delete(&id) {
                    Ok(()) => {
                        self.output.push_system(&format!("已删除人格: {id}"));
                    }
                    Err(e) => {
                        self.output.push_system(&format!("删除失败: {e}"));
                    }
                }
            }
            "create" => {
                // :persona create <id> <name> [description]
                let id = args.get(1);
                let name = args.get(2);
                let desc = args.get(3).map(|s| s.as_str()).unwrap_or("自定义人格");

                match (id, name) {
                    (Some(id), Some(name)) => {
                        let config = brain_memory::persona_types::PersonaConfig::default();
                        match mem.persona_manager_mut().create(
                            id.clone(),
                            name.clone(),
                            desc.to_string(),
                            String::new(), // system_prompt 初始为空
                            config,
                        ) {
                            Ok(persona) => {
                                self.output.push_system(&format!(
                                    "✅ 已创建人格: {} ({})",
                                    persona.name, persona.id
                                ));
                                self.output.push_system("提示: 使用 `/memory manage` 可设置专属 system prompt");
                            }
                            Err(e) => {
                                self.output.push_system(&format!("创建失败: {e}"));
                            }
                        }
                    }
                    _ => {
                        self.output.push_system("用法: :persona create <id> <name> [description]");
                        self.output.push_system("示例: :persona create writer 滚开作家 网文写作助手");
                    }
                }
            }
            other => {
                self.output.push_system(&format!(
                    "未知子命令: {other}。可用: list, switch, info, create, delete"
                ));
            }
        }
        HandleResult::Handled
    }

    /// 记忆管理命令拦截
    fn handle_memory_command(&mut self, args: &[String]) -> HandleResult {
        let sub = match args.first() {
            Some(s) => s.as_str(),
            None => {
                self.output.push_system("用法: :memory <stats|recall|save|daily> [参数]");
                return HandleResult::Handled;
            }
        };

        let mem_arc = self.orch.memory_brain();
        let mem = match mem_arc.try_lock() {
            Ok(m) => m,
            Err(_) => {
                self.output.push_system("记忆脑正忙，请稍后重试");
                return HandleResult::Handled;
            }
        };

        match sub {
            "stats" => {
                match mem.stats() {
                    Ok(s) => {
                        self.output.push_system("=== 金字塔记忆统计 ===");
                        self.output.push_system(&format!("  L1 全量基座: {} 条", s.l1_count));
                        self.output.push_system(&format!("  L2 任务摘要: {} 条", s.l2_count));
                        self.output.push_system(&format!("  L3 经验条目: {} 条", s.l3_count));
                        self.output.push_system(&format!("  L4 潜意识: {}", if s.l4_exists { "已生成" } else { "未生成" }));
                        self.output.push_system(&format!("  活跃人格: {}", s.active_persona));
                        self.output.push_system(&format!("  会话ID: {}", s.session_id));
                    }
                    Err(e) => self.output.push_system(&format!("统计读取失败: {e}")),
                }
            }
            "recall" => {
                let query = match args.get(1) {
                    Some(q) => q,
                    None => {
                        self.output.push_system("用法: :memory recall <query>");
                        return HandleResult::Handled;
                    }
                };
                let results = mem.recall_for_context(query, 5);
                if results.is_empty() {
                    self.output.push_system(&format!("未找到与 '{}' 相关的记忆", query));
                } else {
                    self.output.push_system(&format!("找到 {} 条相关记忆:", results.len()));
                    for (i, entry) in results.iter().enumerate() {
                        let preview = if entry.content.len() > 120 {
                            format!("{}...", &entry.content[..120])
                        } else {
                            entry.content.clone()
                        };
                        self.output.push_system(&format!("  {}. {}", i + 1, preview));
                    }
                }
            }
            "save" => {
                let text = match args.get(1) {
                    Some(t) => t,
                    None => {
                        self.output.push_system("用法: :memory save <text>");
                        return HandleResult::Handled;
                    }
                };
                match mem.store_turn("User", &format!("[手动保存] {text}"), None) {
                    Ok(()) => self.output.push_system("已保存到 L1 基座"),
                    Err(e) => self.output.push_system(&format!("保存失败: {e}")),
                }
            }
            "daily" => {
                // 从 L2 读取最近摘要
                let summaries = mem.list_recent_summaries(5);
                if summaries.is_empty() {
                    self.output.push_system("暂无任务摘要");
                } else {
                    self.output.push_system(&format!("最近 {} 条任务摘要:", summaries.len()));
                    for s in &summaries {
                        let preview = if s.summary_preview.len() > 100 {
                            format!("{}...", &s.summary_preview[..100])
                        } else {
                            s.summary_preview.clone()
                        };
                        self.output.push_system(&format!("  [{}] {}", s.session_start, preview));
                    }
                }
            }
            other => {
                self.output.push_system(&format!(
                    "未知子命令: {other}。可用: stats, recall, save, daily"
                ));
            }
        }
        HandleResult::Handled
    }

    /// 进化脑命令拦截
    fn handle_evo_command(&mut self, args: &[String]) -> HandleResult {
        let sub = args.first().map(|s| s.as_str());

        match sub {
            Some("status") | None => {
                // 使用 tokio::task::block_in_place 在同步上下文中调用 async 方法
                let status = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(self.orch.evolution_status())
                });
                for line in status.lines() {
                    self.output.push_system(line);
                }
            }
            Some("approve") => {
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(self.orch.approve_evolution())
                });
                match result {
                    Ok(()) => self.output.push_system("\x1b[32m进化变更已批准\x1b[0m"),
                    Err(e) => self.output.push_system(&format!("批准失败: {e}")),
                }
            }
            Some("reject") => {
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(self.orch.reject_evolution())
                });
                match result {
                    Ok(()) => self.output.push_system("进化变更已拒绝"),
                    Err(e) => self.output.push_system(&format!("拒绝失败: {e}")),
                }
            }
            Some("diff") => {
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(self.orch.evolution_diff())
                });
                match result {
                    Ok(diff) => {
                        for line in diff.lines() {
                            self.output.push_system(line);
                        }
                    }
                    Err(e) => self.output.push_system(&format!("获取差异失败: {e}")),
                }
            }
            Some(goal) => {
                let goal = goal.to_string();
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(self.orch.start_evolution(goal))
                });
                match result {
                    Ok(msg) => self.output.push_system(&format!("进化任务已启动: {msg}")),
                    Err(e) => self.output.push_system(&format!("启动失败: {e}")),
                }
            }
        }
        HandleResult::Handled
    }

    /// MCP 命令拦截
    fn handle_mcp_command(&mut self, args: &[String]) -> HandleResult {
        let sub = match args.first() {
            Some(s) => s.as_str(),
            None => {
                self.output.push_system("用法: :mcp <list|status|reconnect> [参数]");
                return HandleResult::Handled;
            }
        };

        let pool = self.orch.mcp_pool();
        match sub {
            "list" => {
                let servers = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(pool.list_servers())
                });
                if servers.is_empty() {
                    self.output.push_system("暂无已配置的 MCP 服务");
                    self.output.push_system("  配置文件: ~/.ai-brain/mcp_servers.json");
                } else {
                    self.output.push_system(&format!("MCP 服务 ({}个):", servers.len()));
                    for (name, status) in &servers {
                        let status_str = match status {
                            ServerStatus::Connected => "\x1b[32m已连接\x1b[0m",
                            ServerStatus::Disconnected => "\x1b[33m未连接\x1b[0m",
                            ServerStatus::Error(e) => &format!("\x1b[31m错误: {e}\x1b[0m"),
                        };
                        self.output.push_system(&format!(
                            "  \x1b[36m{}\x1b[0m — {}",
                            name, status_str
                        ));
                    }
                }
            }
            "status" => {
                let name = match args.get(1) {
                    Some(n) => n.clone(),
                    None => {
                        self.output.push_system("用法: :mcp status <server>");
                        return HandleResult::Handled;
                    }
                };
                let status = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(pool.server_status(&name))
                });
                match status {
                    Some(s) => {
                        let status_str = match &s {
                            ServerStatus::Connected => "已连接",
                            ServerStatus::Disconnected => "未连接",
                            ServerStatus::Error(e) => &format!("错误: {e}"),
                        };
                        self.output.push_system(&format!(
                            "MCP 服务 \x1b[36m{}\x1b[0m: {}",
                            name, status_str
                        ));
                    }
                    None => {
                        self.output.push_system(&format!("未找到 MCP 服务: {name}"));
                    }
                }
            }
            "reconnect" => {
                let name = match args.get(1) {
                    Some(n) => n,
                    None => {
                        self.output.push_system("用法: :mcp reconnect <server>");
                        return HandleResult::Handled;
                    }
                };
                match tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(pool.reconnect(name))
                }) {
                    Ok(()) => {
                        self.output.push_system(&format!("✅ MCP 服务 '{name}' 已重置，下次使用时自动重连"));
                    }
                    Err(e) => {
                        self.output.push_system(&format!("重连失败: {e}"));
                    }
                }
            }
            other => {
                self.output.push_system(&format!(
                    "未知子命令: {other}。可用: list, status, reconnect"
                ));
            }
        }
        HandleResult::Handled
    }

    /// 插件管理命令拦截
    fn handle_plugin_command(&mut self, args: &[String]) -> HandleResult {
        let sub = match args.first() {
            Some(s) => s.as_str(),
            None => {
                self.output.push_system("用法: :plugin <list|install|uninstall> [参数]");
                return HandleResult::Handled;
            }
        };

        match sub {
            "list" => {
                match self.orch.plugin_mgr() {
                    Some(mgr) => {
                        let plugins = mgr.list();
                        if plugins.is_empty() {
                            self.output.push_system("暂无已安装插件");
                            self.output.push_system("  使用 :plugin install <path> 安装");
                        } else {
                            self.output.push_system(&format!("已安装插件 ({}个):", plugins.len()));
                            for p in plugins {
                                self.output.push_system(&format!(
                                    "  {} ({}/{}) v{}",
                                    p.name, p.publisher, p.source, p.version
                                ));
                            }
                        }
                    }
                    None => self.output.push_system("插件管理器未初始化"),
                }
            }
            "install" => {
                let _source = match args.get(1) {
                    Some(s) => s.clone(),
                    None => {
                        self.output.push_system("用法: :plugin install <path>");
                        self.output.push_system("  path 是本地插件目录路径");
                        return HandleResult::Handled;
                    }
                };
                self.output.push_system("⚠️ 插件安装需要在启动前完成，请手动编辑 ~/.ai-brain/plugins/registry.json");
            }
            "uninstall" => {
                let _name = match args.get(1) {
                    Some(n) => n.clone(),
                    None => {
                        self.output.push_system("用法: :plugin uninstall <name>");
                        return HandleResult::Handled;
                    }
                };
                self.output.push_system("⚠️ 插件卸载需要在启动前完成，请手动编辑 ~/.ai-brain/plugins/registry.json");
            }
            other => {
                self.output.push_system(&format!(
                    "未知子命令: {other}。可用: list, install, uninstall"
                ));
            }
        }
        HandleResult::Handled
    }
}

/// 命令处理结果（App 内部使用，与 command::CommandResult 区分）
enum HandleResult {
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

#[cfg(test)]
mod tests {
    use super::*;

    // ─── wrap_text_to_width 测试 ───

    #[test]
    fn wrap_ascii_short_line_no_wrap() {
        let result = App::wrap_text_to_width("Hello", 80);
        assert_eq!(result, vec!["Hello"]);
    }

    #[test]
    fn wrap_ascii_exact_width() {
        let result = App::wrap_text_to_width("12345", 5);
        assert_eq!(result, vec!["12345"]);
    }

    #[test]
    fn wrap_ascii_overflow() {
        let result = App::wrap_text_to_width("1234567890", 5);
        assert_eq!(result, vec!["12345", "67890"]);
    }

    #[test]
    fn wrap_cjk_basic() {
        // 每个 CJK 字符宽度 2，max_width=6 可以放 3 个 CJK 字符
        let result = App::wrap_text_to_width("你好世界再见", 6);
        assert_eq!(result, vec!["你好世", "界再见"]);
    }

    #[test]
    fn wrap_mixed_ascii_cjk() {
        // "> 你好" → "> " (2) + "你好" (4) = 6，max_width=6 刚好
        let result = App::wrap_text_to_width("> 你好", 6);
        assert_eq!(result, vec!["> 你好"]);
    }

    #[test]
    fn wrap_cjk_overflow() {
        // "> 你好世界" → width 10, max_width=6 → "> 你好" (6) + "世界" (4)
        let result = App::wrap_text_to_width("> 你好世界", 6);
        assert_eq!(result, vec!["> 你好", "世界"]);
    }

    #[test]
    fn wrap_empty() {
        let result = App::wrap_text_to_width("", 80);
        assert_eq!(result, vec![""]);
    }

    // ─── screen_col_to_char_idx 测试 ───

    #[test]
    fn char_idx_ascii_start() {
        assert_eq!(App::screen_col_to_char_idx("Hello", 0), 0);
    }

    #[test]
    fn char_idx_ascii_middle() {
        assert_eq!(App::screen_col_to_char_idx("Hello", 3), 3); // 'l'
    }

    #[test]
    fn char_idx_ascii_end() {
        assert_eq!(App::screen_col_to_char_idx("Hello", 5), 5); // past end
    }

    #[test]
    fn char_idx_cjk() {
        // "你好世界" → display: 0:你 2:好 4:世 6:界
        assert_eq!(App::screen_col_to_char_idx("你好世界", 0), 0); // 你
        assert_eq!(App::screen_col_to_char_idx("你好世界", 2), 1); // 好
        assert_eq!(App::screen_col_to_char_idx("你好世界", 4), 2); // 世
        assert_eq!(App::screen_col_to_char_idx("你好世界", 6), 3); // 界
    }

    #[test]
    fn char_idx_mixed() {
        // "> 你好" → display: 0:> 1:  2:你 4:好
        assert_eq!(App::screen_col_to_char_idx("> 你好", 0), 0); // >
        assert_eq!(App::screen_col_to_char_idx("> 你好", 2), 2); // 你
        assert_eq!(App::screen_col_to_char_idx("> 你好", 4), 3); // 好
    }

    // ─── 完整选择流程模拟测试 ───

    /// 模拟选择流程：构建 rendered_text，模拟点击，验证提取文本
    #[test]
    fn selection_single_line_ascii() {
        // 模拟 rendered_text 有一行 "Hello World" (width 11)
        let rendered = vec!["Hello World".to_string()];

        // 模拟点击 col=2 ('l'), Shift+Click col=8 ('r')
        let start_col = 2;
        let end_col = 8;

        let line_text = &rendered[0];
        let ci_s = App::screen_col_to_char_idx(line_text, start_col);
        let ci_e = App::screen_col_to_char_idx(line_text, end_col + 1);
        let selected: String = line_text
            .chars()
            .skip(ci_s)
            .take(ci_e.saturating_sub(ci_s))
            .collect();

        assert_eq!(selected, "llo Wor");
    }

    #[test]
    fn selection_single_line_last_chars() {
        // 测试选中行末最后几个字符 — 这是用户报告的问题场景
        let rendered = vec!["Hello World".to_string()]; // width 11

        // 点击 col=6 ('W'), Shift+Click col=10 ('d')
        let start_col = 6;
        let end_col = 10;

        let line_text = &rendered[0];
        let ci_s = App::screen_col_to_char_idx(line_text, start_col);
        let ci_e = App::screen_col_to_char_idx(line_text, end_col + 1);
        let selected: String = line_text
            .chars()
            .skip(ci_s)
            .take(ci_e.saturating_sub(ci_s))
            .collect();

        assert_eq!(ci_s, 6, "start char index should be 6 (W)");
        assert_eq!(ci_e, 11, "end char index should be 11 (past 'd')");
        assert_eq!(selected, "World", "should select 'World'");
    }

    #[test]
    fn selection_single_line_first_char_to_end() {
        // 用户说从第一个字符开始可以选中到行末
        let rendered = vec!["Hello World".to_string()];

        let start_col = 0;
        let end_col = 10;

        let line_text = &rendered[0];
        let ci_s = App::screen_col_to_char_idx(line_text, start_col);
        let ci_e = App::screen_col_to_char_idx(line_text, end_col + 1);
        let selected: String = line_text
            .chars()
            .skip(ci_s)
            .take(ci_e.saturating_sub(ci_s))
            .collect();

        assert_eq!(selected, "Hello World");
    }

    #[test]
    fn selection_multi_line() {
        let rendered = vec![
            "Line one content".to_string(),
            "Line two content".to_string(),
            "Line three content".to_string(),
        ];

        // Start: line 0, col 5
        // End: line 2, col 10
        let start_line = 0;
        let start_col = 5;
        let end_line = 2;
        let end_col = 10;

        let mut parts: Vec<String> = Vec::new();
        for (i, line_text) in rendered.iter().enumerate() {
            if i < start_line || i > end_line {
                continue;
            }
            if start_line == end_line {
                let ci_s = App::screen_col_to_char_idx(line_text, start_col);
                let ci_e = App::screen_col_to_char_idx(line_text, end_col + 1);
                let sel: String = line_text
                    .chars()
                    .skip(ci_s)
                    .take(ci_e.saturating_sub(ci_s))
                    .collect();
                parts.push(sel);
            } else if i == start_line {
                let ci = App::screen_col_to_char_idx(line_text, start_col);
                parts.push(line_text.chars().skip(ci).collect());
            } else if i == end_line {
                let ci = App::screen_col_to_char_idx(line_text, end_col + 1);
                parts.push(line_text.chars().take(ci).collect());
            } else {
                parts.push(line_text.clone());
            }
        }

        let text = parts.join("\n");
        assert_eq!(text, "one content\nLine two content\nLine three ");
    }

    #[test]
    fn selection_cjk_line_end() {
        // 测试 CJK 文本行末选择
        let rendered = vec!["这是一段中文测试文本".to_string()]; // 每个 CJK 宽度2

        // 选中最后4个字符 "试文本"
        // "这"(0-1) "是"(2-3) "一"(4-5) "段"(6-7) "中"(8-9) "文"(10-11) "测"(12-13) "试"(14-15) "文"(16-17) "本"(18-19)
        let start_col = 12; // '测'
        let end_col = 18; // '本' 的起始列
        let line_text = &rendered[0];

        let ci_s = App::screen_col_to_char_idx(line_text, start_col);
        let ci_e = App::screen_col_to_char_idx(line_text, end_col + 1); // past '本'
        let selected: String = line_text
            .chars()
            .skip(ci_s)
            .take(ci_e.saturating_sub(ci_s))
            .collect();

        // 每个屏幕列=2单位, 所以 col 12 → char idx 6 ('测')
        assert_eq!(ci_s, 6, "start at char 6 (测)");
        assert_eq!(selected, "测试文本");
    }

    // ─── 宽度计算测试 ───

    #[test]
    fn display_width_ascii() {
        let w: usize = "Hello"
            .chars()
            .map(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1))
            .sum();
        assert_eq!(w, 5);
    }

    #[test]
    fn display_width_cjk() {
        let w: usize = "你好世界"
            .chars()
            .map(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1))
            .sum();
        assert_eq!(w, 8); // 4 chars × 2 width
    }

    #[test]
    fn display_width_mixed() {
        let w: usize = "> 你好"
            .chars()
            .map(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1))
            .sum();
        assert_eq!(w, 6); // "> " = 2, "你好" = 4
    }
}
