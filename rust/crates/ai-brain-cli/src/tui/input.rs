//! 输入区域组件
//!
//! 封装 tui-textarea + 自动折行层 + 命令历史 + 智能补全。
//!
//! 核心设计（区别于直接使用 tui-textarea）：
//! - `original`: String 真源，不含软折行换行符
//! - `sync_display()`: auto_wrap(original) → 写入 textarea（纯展示）
//! - 所有输入被拦截后写入 original，再 sync 到 textarea
//! - 光标跟踪：byte offset ⇄ (visual_row, visual_col) 映射
//! - 提交时返回 original（只带用户真实换行，软折行不存在于返回值）
//! - 历史浏览：Up/Down 翻阅已提交的输入
//! - Tab 补全：集成 EvolutionCompleter 提供上下文感知补全

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::widgets::block::Position;
use ratatui::widgets::Block;
use tui_textarea::{CursorMove, TextArea};

use super::completion::{
    CompletionPopup, EvolutionCompleter, InputContext,
};

const INPUT_MIN_HEIGHT: u16 = 3;
const INPUT_MAX_HEIGHT: u16 = 10;
/// 粘贴折叠阈值：字符数
const PASTE_COLLAPSE_CHARS: usize = 200;
/// 粘贴折叠阈值：行数
const PASTE_COLLAPSE_LINES: usize = 5;
/// 最大历史记录数
const MAX_HISTORY: usize = 50;

/// `apply_key` 的返回值
#[derive(Debug, PartialEq, Eq)]
pub enum InputResult {
    /// 输入已消费（original 已更新 + textarea 已同步）
    Consumed,
    /// 用户按了 Enter（无修饰键），请求提交
    Submit,
    /// 按键未处理（交给 app 层，如 Ctrl+C/Ctrl+E 等）
    Ignored,
}

pub struct InputArea {
    pub textarea: TextArea<'static>,
    /// 原始文本（真源，不含软折行）
    original: String,
    /// 光标在 original 中的字节偏移
    cursor_byte: usize,
    /// 当前折行宽度（字符数）
    wrap_width: usize,
    /// 长粘贴折叠显示
    paste_collapsed: bool,
    /// 粘贴内容在 original 中的结束位置（0 表示无粘贴）
    /// original[..paste_end_byte] = 粘贴内容，original[paste_end_byte..] = 用户后续输入
    paste_end_byte: usize,
    /// ── 历史管理 ──
    /// 历史记录队列（最新在前）
    history: Vec<String>,
    /// 当前历史浏览索引（None = 不在浏览中，正在编辑新输入）
    history_index: Option<usize>,
    /// 进入历史模式前保存的输入
    history_saved: String,
    /// ── 补全 ──
    /// 补全器
    completer: EvolutionCompleter,
    /// 补全弹出窗口状态
    popup: CompletionPopup,
    /// 补全弹出窗口是否在输入框中显示
    pub completion_hint: Option<String>,
    /// 上下文提示（显示在状态栏旁）
    pub context_hint: Option<String>,
}

impl InputArea {
    pub fn new(completer: EvolutionCompleter) -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::bordered()
                .title(" 输入 ")
                .title_position(Position::Bottom),
        );
        textarea.set_style(Style::default().fg(Color::White));
        textarea.set_cursor_style(Style::default().fg(Color::Cyan));

        Self {
            textarea,
            original: String::new(),
            cursor_byte: 0,
            wrap_width: 80,
            paste_collapsed: false,
            paste_end_byte: 0,
            history: Vec::new(),
            history_index: None,
            history_saved: String::new(),
            completer,
            popup: CompletionPopup::new(),
            completion_hint: None,
            context_hint: None,
        }
    }

    /// 更新补全器数据
    pub fn set_completer(&mut self, completer: EvolutionCompleter) {
        self.completer = completer;
    }

    /// 获取补全器引用
    pub fn completer(&self) -> &EvolutionCompleter {
        &self.completer
    }

    // ─── 公共接口 ──────────────────────────────────────────────────

    /// 获取原始输入内容（不含软折行，保留用户真实换行）
    pub fn input_text(&self) -> String {
        self.original.clone()
    }

    /// 更新折行宽度（终端 resize 时调用）
    pub fn set_wrap_width(&mut self, terminal_width: u16) {
        let new_width = ((terminal_width.saturating_sub(6)) / 2) as usize;
        let new_width = new_width.max(20);
        if new_width != self.wrap_width {
            self.wrap_width = new_width;
            self.sync_display();
        }
    }

    /// 计算期望高度（基于折行后的视觉行数）
    pub fn desired_height(&self) -> u16 {
        if self.paste_collapsed {
            // 折叠时：占位符 + 用户后续输入的实际行数
            let lines = self.textarea.lines();
            let visual = if lines.is_empty() || (lines.len() == 1 && lines[0].is_empty()) {
                1
            } else {
                lines.len()
            };
            return (visual as u16 + 2).clamp(INPUT_MIN_HEIGHT, INPUT_MAX_HEIGHT);
        }
        let lines = self.textarea.lines();
        let visual = if lines.is_empty() || (lines.len() == 1 && lines[0].is_empty()) {
            1
        } else {
            lines.len()
        };
        (visual as u16 + 2).clamp(INPUT_MIN_HEIGHT, INPUT_MAX_HEIGHT)
    }

    /// 清空
    pub fn clear(&mut self) {
        self.original.clear();
        self.cursor_byte = 0;
        self.paste_collapsed = false;
        self.paste_end_byte = 0;
        self.popup.clear();
        self.completion_hint = None;
        self.context_hint = None;
        self.textarea.select_all();
        self.textarea.cut();
        self.exit_history();
    }

    /// 提交：返回 original（用户真实输入），并记录历史
    pub fn submit(&mut self) -> String {
        let text = std::mem::take(&mut self.original);
        // 记录历史（非空且不重复）
        if !text.is_empty() && (self.history.first().map_or(true, |h| h != &text)) {
            self.history.insert(0, text.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.pop();
            }
        }
        self.cursor_byte = 0;
        self.paste_collapsed = false;
        self.paste_end_byte = 0;
        self.popup.clear();
        self.completion_hint = None;
        self.context_hint = None;
        self.exit_history();
        self.textarea.select_all();
        self.textarea.cut();
        text
    }

    /// 获取历史记录
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// 粘贴
    pub fn insert_paste(&mut self, text: &str) {
        let cleaned: String = text.chars().filter(|&c| c != '\r').collect();
        if cleaned.is_empty() {
            return;
        }
        let idx = self.cursor_byte.min(self.original.len());
        self.original.insert_str(idx, &cleaned);
        self.cursor_byte = idx + cleaned.len();
        let chars = self.original.chars().count();
        let lines = self.original.lines().count().max(1);
        if chars > PASTE_COLLAPSE_CHARS || lines > PASTE_COLLAPSE_LINES {
            self.paste_collapsed = true;
            // 记录粘贴内容的结束位置（= 插入点 + 粘贴长度）
            self.paste_end_byte = idx + cleaned.len();
        }
        self.sync_display();
        self.update_completion();
    }

    // ─── 键盘输入 ─────────────────────────────────────────────────

    /// 处理键盘输入
    pub fn apply_key(&mut self, key: KeyEvent) -> InputResult {
        match (key.modifiers, key.code) {
            // ── 可打印字符 ──
            (KeyModifiers::NONE, KeyCode::Char(c)) | (KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                self.exit_history();
                let idx = self.cursor_byte.min(self.original.len());
                self.original.insert(idx, c);
                self.cursor_byte = idx + c.len_utf8();
                self.popup.clear();
                self.sync_display();
                self.update_completion();
                InputResult::Consumed
            }

            // ── 删除 ──
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.exit_history();
                // 折叠状态下，光标在占位符区域或紧邻占位符末尾 → 整块删除粘贴内容
                if self.paste_collapsed && self.cursor_byte <= self.paste_end_byte {
                    let after = self.original[self.paste_end_byte.min(self.original.len())..].to_string();
                    self.original = after;
                    self.cursor_byte = 0;
                    self.paste_collapsed = false;
                    self.paste_end_byte = 0;
                    self.sync_display();
                    self.update_completion();
                    return InputResult::Consumed;
                }
                if let Some(prev) = self.prev_char_boundary() {
                    self.original.drain(prev..self.cursor_byte);
                    self.cursor_byte = prev;
                    // 如果删除后粘贴区域变空，解除折叠
                    if self.paste_collapsed && self.cursor_byte < self.paste_end_byte {
                        self.paste_collapsed = false;
                        self.paste_end_byte = 0;
                    }
                    self.sync_display();
                    self.update_completion();
                }
                InputResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                self.exit_history();
                // 折叠状态下，光标在占位符区域内 → 整块删除粘贴内容
                if self.paste_collapsed && self.cursor_byte < self.paste_end_byte {
                    let after = self.original[self.paste_end_byte.min(self.original.len())..].to_string();
                    self.original = after;
                    self.cursor_byte = 0;
                    self.paste_collapsed = false;
                    self.paste_end_byte = 0;
                    self.sync_display();
                    self.update_completion();
                    return InputResult::Consumed;
                }
                if let Some(next) = self.next_char_boundary() {
                    self.original.drain(self.cursor_byte..next);
                    self.sync_display();
                    self.update_completion();
                }
                InputResult::Consumed
            }

            // ── 词级删除（Ctrl+W / Ctrl+Backspace / Alt+Backspace / Ctrl+Delete） ──
            (KeyModifiers::CONTROL, KeyCode::Char('w'))
            | (KeyModifiers::CONTROL, KeyCode::Backspace)
            | (KeyModifiers::ALT, KeyCode::Backspace) => {
                self.exit_history();
                let start = self.find_prev_word_start(self.cursor_byte);
                self.delete_range(start, self.cursor_byte);
                InputResult::Consumed
            }
            (KeyModifiers::CONTROL, KeyCode::Delete)
            | (KeyModifiers::ALT, KeyCode::Delete) => {
                self.exit_history();
                let end = self.find_next_word_end(self.cursor_byte);
                self.delete_range(self.cursor_byte, end);
                InputResult::Consumed
            }

            // ── 行级删除（Ctrl+U 删到行首，Ctrl+K 删到行尾） ──
            (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
                self.exit_history();
                let line_start = self.find_line_start(self.cursor_byte);
                self.delete_range(line_start, self.cursor_byte);
                InputResult::Consumed
            }
            (KeyModifiers::CONTROL, KeyCode::Char('k')) => {
                self.exit_history();
                let line_end = self.find_line_end(self.cursor_byte);
                self.delete_range(self.cursor_byte, line_end);
                InputResult::Consumed
            }

            // ── 词级跳转（Ctrl+Left / Ctrl+Right / Alt+Left / Alt+Right） ──
            (KeyModifiers::CONTROL, KeyCode::Left)
            | (KeyModifiers::ALT, KeyCode::Left) => {
                self.exit_history();
                self.popup.clear();
                let target = self.find_prev_word_start(self.cursor_byte);
                self.cursor_byte = target;
                self.sync_cursor_only();
                InputResult::Consumed
            }
            (KeyModifiers::CONTROL, KeyCode::Right)
            | (KeyModifiers::ALT, KeyCode::Right) => {
                self.exit_history();
                self.popup.clear();
                let end = self.find_next_word_end(self.cursor_byte);
                // 光标停在词尾后第一个空白之后或下一个词首
                self.cursor_byte = end;
                self.sync_cursor_only();
                InputResult::Consumed
            }

            // ── 光标移动 ──
            (KeyModifiers::NONE, KeyCode::Left) => {
                self.exit_history();
                self.popup.clear();
                self.move_cursor_left();
                InputResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                self.exit_history();
                self.popup.clear();
                self.move_cursor_right();
                InputResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.popup.clear();
                self.navigate_history_older();
                InputResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.popup.clear();
                self.navigate_history_newer();
                InputResult::Consumed
            }
            // Home → 当前视觉行首
            (KeyModifiers::NONE, KeyCode::Home) => {
                self.popup.clear();
                self.move_cursor_home();
                InputResult::Consumed
            }
            // End → 当前视觉行尾
            (KeyModifiers::NONE, KeyCode::End) => {
                self.popup.clear();
                self.move_cursor_end();
                InputResult::Consumed
            }
            // Ctrl+A → 全文开头
            (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
                self.popup.clear();
                self.move_cursor_to_input_start();
                InputResult::Consumed
            }
            // Ctrl+E → 全文末尾
            (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                self.popup.clear();
                self.move_cursor_to_input_end();
                InputResult::Consumed
            }
            // Ctrl+H → 等价 Backspace（部分终端发送此键码）
            (KeyModifiers::CONTROL, KeyCode::Char('h')) => {
                self.exit_history();
                if let Some(prev) = self.prev_char_boundary() {
                    self.original.drain(prev..self.cursor_byte);
                    self.cursor_byte = prev;
                    self.sync_display();
                    self.update_completion();
                }
                InputResult::Consumed
            }

            // ── Enter 系列 ──
            (KeyModifiers::NONE, KeyCode::Enter) => {
                // 如果弹出窗口可见，选中当前项
                if self.popup.visible {
                    self.accept_completion();
                    return InputResult::Consumed;
                }
                self.popup.clear();
                InputResult::Submit
            }
            (KeyModifiers::SHIFT, KeyCode::Enter) => {
                self.exit_history();
                self.popup.clear();
                let idx = self.cursor_byte.min(self.original.len());
                self.original.insert(idx, '\n');
                self.cursor_byte = idx + 1;
                self.sync_display();
                InputResult::Consumed
            }

            // ── Tab → 补全或插入空格 ──
            (KeyModifiers::NONE, KeyCode::Tab) => {
                if self.popup.visible {
                    self.accept_completion();
                } else {
                    self.trigger_completion();
                }
                InputResult::Consumed
            }

            // ── Ctrl+P → 展开/折叠长粘贴 ──
            (KeyModifiers::CONTROL, KeyCode::Char('p')) => {
                if !self.original.is_empty() {
                    self.paste_collapsed = !self.paste_collapsed;
                    self.sync_display();
                }
                InputResult::Consumed
            }

            // ── 补全弹出窗口导航（Tab 选择，备用键） ──
            (KeyModifiers::SHIFT, KeyCode::Tab) => {
                if self.popup.visible {
                    self.popup.select_prev();
                }
                InputResult::Consumed
            }

            // ── 其他 → 交给 app ──
            _ => InputResult::Ignored,
        }
    }

    // ─── 历史管理 ──────────────────────────────────────────────────

    fn exit_history(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.history_saved.clear();
        }
    }

    fn navigate_history_older(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                // 进入历史模式：保存当前输入
                self.history_saved = self.original.clone();
                self.history_index = Some(0);
            }
            Some(idx) if idx + 1 < self.history.len() => {
                self.history_index = Some(idx + 1);
            }
            _ => return, // 已到最旧历史
        }
        self.load_history_entry();
    }

    fn navigate_history_newer(&mut self) {
        match self.history_index {
            None => return,
            Some(0) => {
                // 回到保存的输入
                self.original = std::mem::take(&mut self.history_saved);
                self.history_index = None;
            }
            Some(idx) => {
                self.history_index = Some(idx - 1);
            }
        }
        if self.history_index.is_some() {
            self.load_history_entry();
        } else {
            self.apply_original_to_display();
        }
    }

    fn load_history_entry(&mut self) {
        if let Some(idx) = self.history_index {
            if let Some(entry) = self.history.get(idx) {
                self.original = entry.clone();
                self.cursor_byte = self.original.len();
                self.sync_display();
            }
        }
    }

    fn apply_original_to_display(&mut self) {
        self.cursor_byte = self.original.len().min(self.original.len());
        self.sync_display();
    }

    // ─── 补全 ──────────────────────────────────────────────────────

    /// 触发补全（Tab 键）
    fn trigger_completion(&mut self) {
        let ctx = InputContext::analyze(&self.original, self.cursor_byte);
        let items = self.completer.complete(&ctx, &self.history);
        self.popup.set_items(items);
        self.update_hint(&ctx);
    }

    /// 接受当前选中的补全项
    fn accept_completion(&mut self) {
        if let Some(item) = self.popup.current().cloned() {
            // 用 replacement 替换文本
            // 策略：如果是命令补全，替换整个文本；否则替换当前词
            if item.category == super::completion::CompletionCategory::Command {
                self.original = item.replacement.clone();
            } else {
                // 替换光标前的词
                let before = &self.original[..self.cursor_byte];
                let after = &self.original[self.cursor_byte..];
                let new_before = if let Some(last_space) = before.rfind(' ') {
                    format!("{}{}", &before[..=last_space], item.replacement)
                } else {
                    item.replacement.clone()
                };
                self.original = format!("{}{}", new_before, after);
                self.cursor_byte = new_before.len();
            }
            self.popup.clear();
            self.sync_display();
        }
    }

    /// 更新补全建议（输入变化时自动调用）
    fn update_completion(&mut self) {
        if !self.popup.visible {
            // 不在补全模式，只更新时间提示
            let ctx = InputContext::analyze(&self.original, self.cursor_byte);
            self.update_hint(&ctx);
            return;
        }

        let ctx = InputContext::analyze(&self.original, self.cursor_byte);
        let items = self.completer.complete(&ctx, &self.history);
        if items.is_empty() {
            self.popup.clear();
        } else {
            self.popup.set_items(items);
        }
        self.update_hint(&ctx);
    }

    /// 更新上下文提示
    fn update_hint(&mut self, ctx: &InputContext) {
        self.context_hint = self.completer.hint(ctx);
        if self.popup.visible {
            self.completion_hint = self.popup.current().map(|item| {
                format!(
                    "{}: {}",
                    item.category.label(),
                    item.description.as_deref().unwrap_or(&item.display)
                )
            });
        } else {
            self.completion_hint = None;
        }
    }

    /// 获取补全弹出窗口引用
    pub fn popup(&self) -> &CompletionPopup {
        &self.popup
    }

    // ─── 光标操作（internal） ─────────────────────────────────────

    fn prev_char_boundary(&self) -> Option<usize> {
        if self.cursor_byte == 0 {
            return None;
        }
        let mut pos = self.cursor_byte - 1;
        while pos > 0 && !self.original.is_char_boundary(pos) {
            pos -= 1;
        }
        Some(pos)
    }

    fn next_char_boundary(&self) -> Option<usize> {
        if self.cursor_byte >= self.original.len() {
            return None;
        }
        let mut pos = self.cursor_byte + 1;
        while pos < self.original.len() && !self.original.is_char_boundary(pos) {
            pos += 1;
        }
        Some(pos)
    }

    fn move_cursor_left(&mut self) {
        if let Some(prev) = self.prev_char_boundary() {
            self.cursor_byte = prev;
            self.sync_cursor_only();
        }
    }

    fn move_cursor_right(&mut self) {
        if let Some(next) = self.next_char_boundary() {
            self.cursor_byte = next;
            self.sync_cursor_only();
        }
    }

    fn move_cursor_home(&mut self) {
        let (vrow, _) = self.cursor_visual_position();
        self.cursor_byte = self.byte_at_visual(vrow, 0);
        self.sync_cursor_only();
    }

    fn move_cursor_end(&mut self) {
        let (vrow, _) = self.cursor_visual_position();
        self.cursor_byte = self.byte_at_visual(vrow + 1, 0);
        self.sync_cursor_only();
    }

    fn move_cursor_up(&mut self) {
        let (vrow, vcol) = self.cursor_visual_position();
        if vrow > 0 {
            self.cursor_byte = self.byte_at_visual(vrow - 1, vcol);
            self.sync_cursor_only();
        }
    }

    fn move_cursor_down(&mut self) {
        let (vrow, vcol) = self.cursor_visual_position();
        let total = self.total_visual_rows();
        if vrow + 1 < total {
            self.cursor_byte = self.byte_at_visual(vrow + 1, vcol);
        } else {
            self.cursor_byte = self.original.len();
        }
        self.sync_cursor_only();
    }

    /// 跳到全文最开头（Ctrl+A）
    fn move_cursor_to_input_start(&mut self) {
        self.cursor_byte = 0;
        self.sync_cursor_only();
    }

    /// 跳到全文最末尾（Ctrl+E）
    fn move_cursor_to_input_end(&mut self) {
        self.cursor_byte = self.original.len();
        self.sync_cursor_only();
    }

    // ─── 词/行边界 ─────────────────────────────────────────────────

    /// 找到光标前一个词的起始位置（用于 Ctrl+W / Ctrl+Backspace 删词）
    ///
    /// 行为：跳过光标前的空白字符，再跳过非空白字符，返回该词起始位置。
    /// 如果光标在词中间，则仅删除光标到词首部分。
    fn find_prev_word_start(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        let mut p = pos;

        // 第一步：跳过光标前的空白字符
        while p > 0 {
            // 回退到上一个 char boundary
            let mut boundary = p - 1;
            while boundary > 0 && !self.original.is_char_boundary(boundary) {
                boundary -= 1;
            }
            let ch = self.original[boundary..].chars().next().unwrap_or(' ');
            if ch.is_whitespace() {
                p = boundary;
            } else {
                break;
            }
        }

        // 第二步：跳过非空白字符，找到词首
        while p > 0 {
            let mut boundary = p - 1;
            while boundary > 0 && !self.original.is_char_boundary(boundary) {
                boundary -= 1;
            }
            let ch = self.original[boundary..].chars().next().unwrap_or(' ');
            if !ch.is_whitespace() {
                p = boundary;
            } else {
                break;
            }
        }

        p
    }

    /// 找到光标后一个词的结束位置（用于 Ctrl+Delete 删词）
    ///
    /// 行为：跳过光标后的非空白字符，返回该词之后的位置。
    fn find_next_word_end(&self, pos: usize) -> usize {
        let len = self.original.len();
        if pos >= len {
            return len;
        }

        // 跳过非空白字符
        let mut p = pos;
        for ch in self.original[pos..].chars() {
            if ch.is_whitespace() {
                break;
            }
            p += ch.len_utf8();
        }

        // 再跳过一个空白字符（删词后不留多余空格）
        if p < len {
            let ch = self.original[p..].chars().next().unwrap_or(' ');
            if ch.is_whitespace() {
                p += ch.len_utf8();
            }
        }

        p
    }

    /// 找到光标所在行的行首位置（用于 Ctrl+U 删到行首）
    fn find_line_start(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        self.original[..pos]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    /// 找到光标所在行的行尾位置（用于 Ctrl+K 删到行尾）
    fn find_line_end(&self, pos: usize) -> usize {
        self.original[pos..]
            .find('\n')
            .map(|i| pos + i)
            .unwrap_or(self.original.len())
    }

    /// 通用删除方法：删除 original 中 [start, end) 范围并同步显示
    fn delete_range(&mut self, start: usize, end: usize) {
        let start = start.min(self.original.len());
        let end = end.min(self.original.len());
        if start < end {
            self.original.drain(start..end);
            self.cursor_byte = start;
            self.sync_display();
            self.update_completion();
        }
    }

    // ─── 视觉位置映射 ──────────────────────────────────────────────

    fn cursor_visual_position(&self) -> (usize, usize) {
        Self::map_byte_to_visual(&self.original, self.cursor_byte, self.wrap_width)
    }

    fn total_visual_rows(&self) -> usize {
        if self.original.is_empty() {
            return 1;
        }
        let (last_row, _) =
            Self::map_byte_to_visual(&self.original, self.original.len(), self.wrap_width);
        last_row + 1
    }

    fn map_byte_to_visual(original: &str, target_byte: usize, wrap_width: usize) -> (usize, usize) {
        let mut vrow: usize = 0;
        let mut vcol: usize = 0;
        let mut line_chars: usize = 0;
        let mut byte_pos: usize = 0;

        for ch in original.chars() {
            if byte_pos >= target_byte {
                break;
            }
            if ch == '\n' {
                vrow += 1;
                vcol = 0;
                line_chars = 0;
            } else {
                vcol += 1;
                line_chars += 1;
                if line_chars >= wrap_width {
                    vrow += 1;
                    vcol = 0;
                    line_chars = 0;
                }
            }
            byte_pos += ch.len_utf8();
        }
        (vrow, vcol)
    }

    fn byte_at_visual(&self, target_row: usize, target_col: usize) -> usize {
        let wrap_width = self.wrap_width;
        let mut vrow: usize = 0;
        let mut vcol: usize = 0;
        let mut line_chars: usize = 0;
        let mut byte_pos: usize = 0;

        for ch in self.original.chars() {
            if vrow > target_row || (vrow == target_row && vcol >= target_col) {
                break;
            }
            if ch == '\n' {
                if vrow == target_row {
                    break;
                }
                vrow += 1;
                vcol = 0;
                line_chars = 0;
            } else {
                if vrow == target_row && vcol == target_col {
                    break;
                }
                vcol += 1;
                line_chars += 1;
                if line_chars >= wrap_width {
                    vrow += 1;
                    vcol = 0;
                    line_chars = 0;
                }
            }
            byte_pos += ch.len_utf8();
        }
        byte_pos
    }

    // ─── 显示同步 ─────────────────────────────────────────────────

    fn sync_display(&mut self) {
        if self.paste_collapsed {
            // 只统计粘贴部分的字符数（不含用户后续输入）
            let paste_text = &self.original[..self.paste_end_byte.min(self.original.len())];
            let char_count = paste_text.chars().count();
            let line_count = paste_text.lines().count().max(1);
            let placeholder = format!("[已粘贴 {char_count} 字符, {line_count} 行 — Ctrl+P 展开]");

            // 用户后续输入 = 粘贴之后的内容
            let after_paste = &self.original[self.paste_end_byte.min(self.original.len())..];
            let display_text = format!("{}{}", placeholder, after_paste);

            let current = self.textarea.lines().join("\n");
            if current != display_text {
                let block = self.textarea.block().cloned();
                let style = self.textarea.style();
                let cursor_style = self.textarea.cursor_style();

                let mut new_ta: TextArea = display_text.lines().map(String::from).collect();
                if let Some(b) = block {
                    new_ta.set_block(b);
                }
                new_ta.set_style(style);
                new_ta.set_cursor_style(cursor_style);
                self.textarea = new_ta;
            }

            // 光标映射：在折叠模式下，光标位置 = 占位符区域偏移 + (cursor_byte - paste_end_byte)
            let cursor_in_paste = self.cursor_byte <= self.paste_end_byte;
            if cursor_in_paste {
                // 光标还在粘贴区域内 → 放到占位符末尾
                let col = placeholder.chars().count();
                self.textarea.move_cursor(CursorMove::Jump(0, col as u16));
            } else {
                // 光标在用户后续输入区域
                let after_offset = self.cursor_byte - self.paste_end_byte;
                let full_line = format!("{}{}", placeholder, after_paste);
                // 计算 placeholder 后 after_offset 字符对应的视觉位置
                let (vrow, vcol) =
                    Self::map_byte_to_visual(&full_line, placeholder.len() + after_offset, self.wrap_width);
                self.textarea.move_cursor(CursorMove::Jump(vrow as u16, vcol as u16));
            }
            return;
        }

        let wrapped = auto_wrap(&self.original, self.wrap_width);
        let current = self.textarea.lines().join("\n");
        if current != wrapped {
            let block = self.textarea.block().cloned();
            let style = self.textarea.style();
            let cursor_style = self.textarea.cursor_style();

            let mut new_ta: TextArea = wrapped.lines().map(String::from).collect();
            if let Some(b) = block {
                new_ta.set_block(b);
            }
            new_ta.set_style(style);
            new_ta.set_cursor_style(cursor_style);
            self.textarea = new_ta;
        }
        let (vrow, vcol) = self.cursor_visual_position();
        self.textarea
            .move_cursor(CursorMove::Jump(vrow as u16, vcol as u16));
    }

    fn sync_cursor_only(&mut self) {
        if self.paste_collapsed {
            // 折叠模式下也需要更新光标位置
            let paste_text = &self.original[..self.paste_end_byte.min(self.original.len())];
            let char_count = paste_text.chars().count();
            let line_count = paste_text.lines().count().max(1);
            let placeholder = format!("[已粘贴 {char_count} 字符, {line_count} 行 — Ctrl+P 展开]");
            let after_paste = &self.original[self.paste_end_byte.min(self.original.len())..];

            let cursor_in_paste = self.cursor_byte <= self.paste_end_byte;
            if cursor_in_paste {
                let col = placeholder.chars().count();
                self.textarea.move_cursor(CursorMove::Jump(0, col as u16));
            } else {
                let after_offset = self.cursor_byte - self.paste_end_byte;
                let full_line = format!("{}{}", placeholder, after_paste);
                let (vrow, vcol) =
                    Self::map_byte_to_visual(&full_line, placeholder.len() + after_offset, self.wrap_width);
                self.textarea.move_cursor(CursorMove::Jump(vrow as u16, vcol as u16));
            }
            return;
        }
        let (vrow, vcol) = self.cursor_visual_position();
        self.textarea
            .move_cursor(CursorMove::Jump(vrow as u16, vcol as u16));
    }
}

// keep the Default impl for backward compatibility but it's now unused
impl Default for InputArea {
    fn default() -> Self {
        Self::new(EvolutionCompleter::lightweight())
    }
}

// ─── 自动折行 ──────────────────────────────────────────────────────

pub fn auto_wrap(text: &str, max_chars: usize) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(text.len() + text.len() / max_chars.max(1));
    for line in text.lines() {
        let char_count = line.chars().count();
        if char_count <= max_chars {
            out.push_str(line);
        } else {
            for (i, ch) in line.chars().enumerate() {
                if i > 0 && i % max_chars == 0 {
                    out.push('\n');
                }
                out.push(ch);
            }
        }
        out.push('\n');
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_line_not_wrapped() {
        assert_eq!(auto_wrap("hello", 40), "hello");
    }

    #[test]
    fn long_line_wrapped() {
        let long = "a".repeat(100);
        let wrapped = auto_wrap(&long, 40);
        assert_eq!(wrapped.lines().count(), 3);
        assert_eq!(wrapped.lines().next().unwrap().chars().count(), 40);
    }

    // ─── 历史测试 ─────────────────────────────────────────────────

    #[test]
    fn test_history_records_submitted() {
        let mut area = InputArea::new(EvolutionCompleter::lightweight());
        area.original = "hello".into();
        let result = area.submit();
        assert_eq!(result, "hello");
        assert_eq!(area.history().len(), 1);
        assert_eq!(area.history()[0], "hello");
    }

    #[test]
    fn test_history_dedup() {
        let mut area = InputArea::new(EvolutionCompleter::lightweight());
        area.original = "cmd".into();
        area.submit();
        area.original = "cmd".into();
        area.submit();
        assert_eq!(area.history().len(), 1); // 去重
    }

    #[test]
    fn test_history_max_size() {
        let mut area = InputArea::new(EvolutionCompleter::lightweight());
        for i in 0..60 {
            area.original = format!("cmd{i}");
            area.submit();
        }
        assert_eq!(area.history().len(), 50);
        assert_eq!(area.history()[0], "cmd59");
    }

    #[test]
    fn test_history_navigation() {
        let mut area = InputArea::new(EvolutionCompleter::lightweight());
        area.original = "first".into(); area.submit();
        area.original = "second".into(); area.submit();
        area.original = "third".into(); area.submit();

        // 当前在编辑新输入
        area.original = "editing".into();
        area.cursor_byte = 7;

        // Up: 进入历史，显示最新
        area.navigate_history_older();
        assert_eq!(area.history_index, Some(0));
        assert_eq!(area.original, "third");

        // 再 Up: 更旧
        area.navigate_history_older();
        assert_eq!(area.original, "second");

        // Down: 回到 "third"
        area.navigate_history_newer();
        assert_eq!(area.original, "third");

        // Down: 退出历史
        area.navigate_history_newer();
        assert_eq!(area.history_index, None);
        assert_eq!(area.original, "editing");
    }

    // ─── 补全测试 ─────────────────────────────────────────────────

    #[test]
    fn test_completion_command() {
        let mut area = InputArea::new(EvolutionCompleter::lightweight());
        area.original = ":h".into();
        area.cursor_byte = 2;
        area.trigger_completion();
        assert!(area.popup.visible);
        assert!(area.popup.items.iter().any(|i| i.display == ":help"));

        area.accept_completion();
        assert_eq!(area.original, ":help");
    }

    #[test]
    fn test_completion_then_submit() {
        // Tab 补全弹出 → Enter 选中第一个
        let mut area = InputArea::new(EvolutionCompleter::lightweight());
        area.original = ":st".into();
        area.cursor_byte = 3;
        area.trigger_completion();
        assert!(area.popup.visible);

        // Enter 选中补全项（不提交）
        let result = area.apply_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(result, InputResult::Consumed); // 补全消耗了 Enter，不是提交
        assert!(!area.popup.visible);
        assert_eq!(area.original, ":status");
    }

    #[test]
    fn test_completion_history_mixed() {
        let mut area = InputArea::new(EvolutionCompleter::lightweight());
        area.history.push("hello world".into());
        area.history.push("help".into());

        area.original = "hel".into();
        area.cursor_byte = 3;
        area.trigger_completion();

        // 应该同时有命令建议和历史匹配
        let has_help_cmd = area.popup.items.iter().any(|i| i.display == ":help");
        let has_help_hist = area.popup.items.iter().any(|i| i.display == "help");
        assert!(has_help_cmd || has_help_hist);
    }

    // ─── 原有测试保留 ─────────────────────────────────────────────

    fn shifted_char_preserved() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut area = InputArea::new(EvolutionCompleter::lightweight());
        area.wrap_width = 80;

        let result = area.apply_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        assert_eq!(result, InputResult::Consumed);
        assert_eq!(area.original, "S");

        area.apply_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        area.apply_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        area.apply_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        area.apply_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        assert_eq!(area.original, "Step1");
        assert_eq!(area.submit(), "Step1");
    }

    // ─── 词边界测试 ────────────────────────────────────────────────

    fn setup_area(text: &str, cursor: usize) -> InputArea {
        let mut area = InputArea::new(EvolutionCompleter::lightweight());
        area.wrap_width = 80;
        area.original = text.to_string();
        area.cursor_byte = cursor;
        area
    }

    #[test]
    fn test_find_prev_word_start_mid_word() {
        let area = setup_area("hello world", 8); // cursor at 'r' in "world"
        let pos = area.find_prev_word_start(8);
        assert_eq!(pos, 6); // start of "world"
    }

    #[test]
    fn test_find_prev_word_start_after_space() {
        let area = setup_area("hello world", 6); // cursor at space after "hello "
        let pos = area.find_prev_word_start(6);
        assert_eq!(pos, 0); // start of "hello"
    }

    #[test]
    fn test_find_prev_word_start_at_word_start() {
        let area = setup_area("hello world", 0); // cursor at 'h'
        let pos = area.find_prev_word_start(0);
        assert_eq!(pos, 0);
    }

    #[test]
    fn test_find_next_word_end() {
        let area = setup_area("hello world", 0); // cursor at 'h'
        let pos = area.find_next_word_end(0);
        assert_eq!(pos, 6); // after "hello" (includes trailing space)
    }

    #[test]
    fn test_find_line_start() {
        let area = setup_area("line1\nline2", 8); // cursor at 'i' in "line2"
        let pos = area.find_line_start(8);
        assert_eq!(pos, 6); // after '\n'
    }

    #[test]
    fn test_find_line_end() {
        let area = setup_area("line1\nline2", 0); // cursor at 'l' in "line1"
        let pos = area.find_line_end(0);
        assert_eq!(pos, 5); // before '\n'
    }

    #[test]
    fn test_ctrl_w_delete_prev_word() {
        let mut area = setup_area("hello world foo", 15); // cursor after "foo"
        area.apply_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(area.original, "hello world ");
        assert_eq!(area.cursor_byte, 12);
    }

    #[test]
    fn test_ctrl_u_delete_to_line_start() {
        let mut area = setup_area("hello world", 10);
        area.apply_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(area.original, "d");  // "hello wor" deleted, "d" remains
        assert_eq!(area.cursor_byte, 0);
    }

    #[test]
    fn test_ctrl_k_delete_to_line_end() {
        let mut area = setup_area("hello world", 0);
        area.apply_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(area.original, "");
        assert_eq!(area.cursor_byte, 0);
    }

    #[test]
    fn test_ctrl_left_jump_word() {
        let mut area = setup_area("hello world", 8); // cursor at 'r' in "world"
        area.apply_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(area.cursor_byte, 6); // start of "world"
    }

    #[test]
    fn test_delete_range_noop_on_equal() {
        let mut area = setup_area("hello", 2);
        area.delete_range(2, 2); // start == end
        assert_eq!(area.original, "hello");
        assert_eq!(area.cursor_byte, 2);
    }

    #[test]
    fn test_ctrl_backspace_delete_prev_word() {
        let mut area = setup_area("hello world foo", 15);
        area.apply_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(area.original, "hello world ");
        assert_eq!(area.cursor_byte, 12);
    }
}
