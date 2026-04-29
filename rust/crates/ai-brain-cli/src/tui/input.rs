//! 输入区域组件
//!
//! 封装 tui-textarea + 自动折行层。
//!
//! 核心设计（区别于直接使用 tui-textarea）：
//! - `original`: String 真源，不含软折行换行符
//! - `sync_display()`: auto_wrap(original) → 写入 textarea（纯展示）
//! - 所有输入被拦截后写入 original，再 sync 到 textarea
//! - 光标跟踪：byte offset ⇄ (visual_row, visual_col) 映射
//! - 提交时返回 original（只带用户真实换行，软折行不存在于返回值）
//!
//! 粘贴路径：
//!   1. Event::Paste（bracketed paste）→ insert_paste → 写入 original → sync_display
//!   2. 普通按键 → apply_key → 写入 original → sync_display

use crossterm::event::{KeyCode, KeyModifiers, KeyEvent};
use ratatui::style::{Color, Style};
use ratatui::widgets::block::Position;
use ratatui::widgets::Block;
use tui_textarea::{CursorMove, TextArea};

const INPUT_MIN_HEIGHT: u16 = 3;
const INPUT_MAX_HEIGHT: u16 = 10;
/// 粘贴折叠阈值：字符数
const PASTE_COLLAPSE_CHARS: usize = 200;
/// 粘贴折叠阈值：行数
const PASTE_COLLAPSE_LINES: usize = 5;

/// `apply_key` 的返回值
#[derive(Debug, PartialEq, Eq)]
pub enum InputResult {
    /// 输入已消费（original 已更新 + textarea 已同步）
    Consumed,
    /// 用户按了 Enter（无修饰键），请求提交
    Submit,
    /// 按键未处理（交给 app 层，如 Ctrl+C / Ctrl+E 等）
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
}

impl InputArea {
    pub fn new() -> Self {
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
        }
    }

    // ─── 公共接口 ──────────────────────────────────────────────────

    #[allow(dead_code)]
    /// 获取原始输入内容（不含软折行，保留用户真实换行）
    pub fn input_text(&self) -> String {
        self.original.clone()
    }

    /// 更新折行宽度（终端 resize 时调用）
    /// 除以 2 以适应 CJK 双宽字符：每个 CJK 字 ≈ 2 列
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
            return INPUT_MIN_HEIGHT;
        }
        let lines = self.textarea.lines();
        let visual = if lines.is_empty() || (lines.len() == 1 && lines[0].is_empty()) {
            1
        } else {
            lines.len()
        };
        (visual as u16 + 2).clamp(INPUT_MIN_HEIGHT, INPUT_MAX_HEIGHT)
    }

    #[allow(dead_code)]
    /// 清空
    pub fn clear(&mut self) {
        self.original.clear();
        self.cursor_byte = 0;
        self.paste_collapsed = false;
        self.textarea.select_all();
        self.textarea.cut();
    }

    /// 提交：返回 original（用户真实输入）
    pub fn submit(&mut self) -> String {
        let text = std::mem::take(&mut self.original);
        self.cursor_byte = 0;
        self.paste_collapsed = false;
        self.textarea.select_all();
        self.textarea.cut();
        text
    }

    // ─── 粘贴 ──────────────────────────────────────────────────────

    /// 处理粘贴事件（bracketed paste 路径）
    /// 去除 \r 防止回车符导致 terminal 渲染覆盖（\r 使光标回行首，后续字符覆盖开头）
    pub fn insert_paste(&mut self, text: &str) {
        let cleaned: String = text.chars().filter(|&c| c != '\r').collect();
        if cleaned.is_empty() {
            return;
        }
        let idx = self.cursor_byte.min(self.original.len());
        self.original.insert_str(idx, &cleaned);
        self.cursor_byte = idx + cleaned.len();
        // 长文本粘贴自动折叠
        let chars = self.original.chars().count();
        let lines = self.original.lines().count().max(1);
        if chars > PASTE_COLLAPSE_CHARS || lines > PASTE_COLLAPSE_LINES {
            self.paste_collapsed = true;
        }
        self.sync_display();
    }

    // ─── 键盘输入 ─────────────────────────────────────────────────

    /// 处理键盘输入
    ///
    /// 只处理文本编辑类按键；Ctrl+C/Esc/Ctrl+D/Ctrl+E 等交给 app 层。
    pub fn apply_key(&mut self, key: KeyEvent) -> InputResult {
        match (key.modifiers, key.code) {
            // ── 可打印字符（含 Shift 修饰，如大写字母/标点）──
            (KeyModifiers::NONE, KeyCode::Char(c))
            | (KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                let idx = self.cursor_byte.min(self.original.len());
                self.original.insert(idx, c);
                self.cursor_byte = idx + c.len_utf8();
                self.sync_display();
                InputResult::Consumed
            }

            // ── 删除 ──
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                if let Some(prev) = self.prev_char_boundary() {
                    self.original.drain(prev..self.cursor_byte);
                    self.cursor_byte = prev;
                    self.sync_display();
                }
                InputResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                if let Some(next) = self.next_char_boundary() {
                    self.original.drain(self.cursor_byte..next);
                    self.sync_display();
                }
                InputResult::Consumed
            }

            // ── 光标移动 ──
            (KeyModifiers::NONE, KeyCode::Left) => {
                self.move_cursor_left();
                InputResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                self.move_cursor_right();
                InputResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.move_cursor_up();
                InputResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.move_cursor_down();
                InputResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::Home)
            | (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
                self.move_cursor_home();
                InputResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                self.move_cursor_end();
                InputResult::Consumed
            }

            // ── Enter 系列 ──
            (KeyModifiers::NONE, KeyCode::Enter) => InputResult::Submit,
            (KeyModifiers::SHIFT, KeyCode::Enter) => {
                let idx = self.cursor_byte.min(self.original.len());
                self.original.insert(idx, '\n');
                self.cursor_byte = idx + 1;
                self.sync_display();
                InputResult::Consumed
            }

            // ── Tab → 4 空格 ──
            (KeyModifiers::NONE, KeyCode::Tab) => {
                let idx = self.cursor_byte.min(self.original.len());
                self.original.insert_str(idx, "    ");
                self.cursor_byte = idx + 4;
                self.sync_display();
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

            // ── 其他 → 交给 app ──
            _ => InputResult::Ignored,
        }
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

    /// 移到当前视觉行首
    fn move_cursor_home(&mut self) {
        let (vrow, _) = self.cursor_visual_position();
        self.cursor_byte = self.byte_at_visual(vrow, 0);
        self.sync_cursor_only();
    }

    /// 移到当前视觉行尾（= 下一视觉行首的字节偏移）
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

    /// 将 original 中的字节偏移映射到折行后的 (row, col)
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

    /// 将视觉位置 (row, col) 映射回 original 中的字节偏移
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
                    break; // 硬换行结束当前视觉行
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

    /// 同步：original → auto_wrap → 重建 textarea（避免 select_all+cut 的状态污染）
    fn sync_display(&mut self) {
        if self.paste_collapsed {
            // 折叠模式：显示占位符
            let char_count = self.original.chars().count();
            let line_count = self.original.lines().count().max(1);
            let placeholder =
                format!("[已粘贴 {char_count} 字符, {line_count} 行 — Ctrl+P 展开]");
            let current = self.textarea.lines().join("\n");
            if current != placeholder {
                let block = self.textarea.block().cloned();
                let style = self.textarea.style();
                let cursor_style = self.textarea.cursor_style();

                let mut new_ta: TextArea = std::iter::once(placeholder).collect();
                if let Some(b) = block {
                    new_ta.set_block(b);
                }
                new_ta.set_style(style);
                new_ta.set_cursor_style(cursor_style);
                self.textarea = new_ta;
            }
            let col = self
                .textarea
                .lines()
                .first()
                .map(|l| l.chars().count())
                .unwrap_or(0);
            self.textarea.move_cursor(CursorMove::Jump(0, col as u16));
            return;
        }

        let wrapped = auto_wrap(&self.original, self.wrap_width);
        let current = self.textarea.lines().join("\n");
        if current != wrapped {
            // 保存样式设置
            let block = self.textarea.block().cloned();
            let style = self.textarea.style();
            let cursor_style = self.textarea.cursor_style();

            // 用折行后的文本重建 TextArea
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

    /// 只更新游标（文本未变时调用，降低闪烁）
    fn sync_cursor_only(&mut self) {
        if self.paste_collapsed {
            return;
        }
        let (vrow, vcol) = self.cursor_visual_position();
        self.textarea
            .move_cursor(CursorMove::Jump(vrow as u16, vcol as u16));
    }
}

impl Default for InputArea {
    fn default() -> Self {
        Self::new()
    }
}

// ─── 自动折行 ──────────────────────────────────────────────────────

/// 将超长行拆分为多行（字符级拆分，插入 `\n`）
///
/// 保留原 `\n`（硬换行），在 `max_chars` 字符边界处插入软换行。
/// 仅用于视觉展示；原始内容由 `original` 保管。
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
    // 去掉末尾多余的换行
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

    #[test]
    fn multi_line_preserved() {
        let text = "line1\nline2\nline3";
        assert_eq!(auto_wrap(text, 40), "line1\nline2\nline3");
    }

    #[test]
    fn cjk_wrapped() {
        let long = "你".repeat(100);
        let wrapped = auto_wrap(&long, 40);
        assert_eq!(wrapped.lines().count(), 3);
        assert!(wrapped.contains('\n'));
    }

    #[test]
    fn empty_text() {
        assert_eq!(auto_wrap("", 40), "");
    }

    // ─── 视觉位置映射测试 ─────────────────────────────────────────

    #[test]
    fn visual_position_simple() {
        let (row, col) = InputArea::map_byte_to_visual("hello", 2, 40);
        assert_eq!((row, col), (0, 2));
    }

    #[test]
    fn visual_position_wrapped() {
        // 50 个 'a'，wrap_width=20 → 3 行
        let text = "a".repeat(50);
        // byte 0..20  → row 0
        // byte 20..40 → row 1
        // byte 40..50 → row 2
        assert_eq!(InputArea::map_byte_to_visual(&text, 0, 20), (0, 0));
        assert_eq!(InputArea::map_byte_to_visual(&text, 19, 20), (0, 19));
        assert_eq!(InputArea::map_byte_to_visual(&text, 20, 20), (1, 0));
        assert_eq!(InputArea::map_byte_to_visual(&text, 39, 20), (1, 19));
        assert_eq!(InputArea::map_byte_to_visual(&text, 40, 20), (2, 0));
    }

    #[test]
    fn visual_position_with_hard_newlines() {
        let text = "hello\nworld";
        // byte 0..4 = "hello" (row 0, col 0..4), byte 5 = '\n'
        // '\n' 尚未被处理 → 光标在 '\n' 之前 = (0, 5)
        assert_eq!(InputArea::map_byte_to_visual(text, 5, 40), (0, 5));
        // 处理完 '\n' 之后 → 光标在 'w' = (1, 0)
        assert_eq!(InputArea::map_byte_to_visual(text, 6, 40), (1, 0));
        // byte 10 = 末尾，在 'd' 处 → (1, 4)
        assert_eq!(InputArea::map_byte_to_visual(text, 10, 40), (1, 4));
    }

    #[test]
    fn byte_at_visual_basic() {
        let mut area = InputArea::new();
        area.original = "hello".into();
        area.wrap_width = 40;
        assert_eq!(area.byte_at_visual(0, 0), 0);
        assert_eq!(area.byte_at_visual(0, 3), 3);
        assert_eq!(area.byte_at_visual(0, 5), 5); // 行尾
        assert_eq!(area.byte_at_visual(1, 0), 5); // 下一行首 = 行尾
    }

    #[test]
    fn shifted_char_preserved() {
        // 模拟带 Shift 修饰键的大写字母输入
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut area = InputArea::new();
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

    #[test]
    fn mixed_shift_and_normal_chars() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut area = InputArea::new();
        area.wrap_width = 80;

        // 模拟粘贴 "Hello World" 的逐字符输入
        for (ch, shift) in [
            ('H', true),
            ('e', false),
            ('l', false),
            ('l', false),
            ('o', false),
            (' ', false),
            ('W', true),
            ('o', false),
            ('r', false),
            ('l', false),
            ('d', false),
        ] {
            let modifiers = if shift {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            let result = area.apply_key(KeyEvent::new(KeyCode::Char(ch), modifiers));
            assert_eq!(result, InputResult::Consumed);
        }

        assert_eq!(area.original, "Hello World");
        assert_eq!(area.submit(), "Hello World");
    }

    #[test]
    fn long_cjk_paste_preserves_content() {
        // 模拟用户原文（含中英文混排、特殊标点）
        let text = "【角色扮演】你是一位专业网文写手，擅长把\"章纲\"扩写成\"读者爽点+画面感+埋钩子\"俱全的完整章节。文章内容需要符合人类的阅读习惯，不能省略一些连接词、因果词等导致阅读出现停滞感。禁制出现类似\"这不是.......是.....\"的解释性的句子。【新增前置流程】（必须优先执行）Step1 拆纲：用「剧情关键点」「需铺垫项」两栏列出章纲所有要素。关键点含：冲突/转折/爆点/钩子。铺垫项含：设定、动机、背景、人物关系。Step2 重排：以\"代入→稳→引→炸→勾\"五段节奏，重新排序关键点与铺垫项。输出一份「调整后剧情顺序清单」（≤10 条，每条≤20 字）。Step3 扩写：按新顺序撰写正文，不得跳序、漏项。正文仍须满足下方【总体风格】【结构】【输出要求】全部条款。";
        let mut area = InputArea::new();
        area.wrap_width = 40;

        // 模拟 Event::Paste（bracketed paste 路径）
        area.insert_paste(text);

        // 原始内容必须完整保留
        assert_eq!(area.original, text);
        // 折行后内容字符总数应等于原文（软换行不算）
        let wrapped = auto_wrap(text, 40);
        let wrapped_chars: usize = wrapped.chars().filter(|&c| c != '\n').count();
        let original_chars = text.chars().count();
        assert_eq!(wrapped_chars, original_chars, "折行不应丢失任何字符");

        // submit 返回原文（无软折行）
        assert_eq!(area.submit(), text);
    }

    #[test]
    fn long_paste_char_by_char_preserves_content() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // 含大写字母、标点、中文的混合文本
        let text = "Hello World: 你好世界！Step1 拆纲：【角色扮演】\"Chapter 3\" — 冲突/转折/爆点";
        let mut area = InputArea::new();
        area.wrap_width = 30;

        for ch in text.chars() {
            let is_uppercase = ch.is_ascii_uppercase();
            let needs_shift = is_uppercase
                || matches!(ch, ':' | '"' | '!' | '—');
            let modifiers = if needs_shift {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            let result = area.apply_key(KeyEvent::new(KeyCode::Char(ch), modifiers));
            assert_eq!(
                result,
                InputResult::Consumed,
                "char '{}' should be consumed",
                ch
            );
        }

        assert_eq!(area.original, text, "逐字符粘贴后 original 应与原文一致");
        assert_eq!(area.submit(), text, "submit 返回原文");
    }

    // ─── 长粘贴折叠测试 ─────────────────────────────────────────────

    #[test]
    fn long_paste_collapses_display() {
        let long_text = "a".repeat(300);
        let mut area = InputArea::new();
        area.wrap_width = 40;

        area.insert_paste(&long_text);

        assert_eq!(area.original, long_text);
        assert!(area.paste_collapsed);

        // textarea 应显示占位符
        let display = area.textarea.lines().join("\n");
        assert!(display.contains("300 字符"));
        assert!(display.contains("Ctrl+P"));

        // submit 返回完整内容
        assert_eq!(area.submit(), long_text);
    }

    #[test]
    fn short_paste_does_not_collapse() {
        let short_text = "hello world";
        let mut area = InputArea::new();
        area.wrap_width = 40;

        area.insert_paste(short_text);

        assert_eq!(area.original, short_text);
        assert!(!area.paste_collapsed);
    }

    #[test]
    fn multi_line_paste_collapses() {
        // 6 行 > 5 行阈值
        let text = "line1\nline2\nline3\nline4\nline5\nline6";
        let mut area = InputArea::new();
        area.wrap_width = 40;

        area.insert_paste(text);

        assert!(area.paste_collapsed);
        assert_eq!(area.submit(), text);
    }

    #[test]
    fn ctrl_p_toggles_collapse() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let long_text = "a".repeat(300);
        let mut area = InputArea::new();
        area.wrap_width = 40;

        area.insert_paste(&long_text);
        assert!(area.paste_collapsed);

        // Ctrl+P 展开
        let result = area.apply_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(result, InputResult::Consumed);
        assert!(!area.paste_collapsed);

        // 展开后 textarea 显示实际内容
        let display = area.textarea.lines().join("\n");
        assert!(!display.contains("Ctrl+P"));

        // Ctrl+P 再次折叠
        let result = area.apply_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(result, InputResult::Consumed);
        assert!(area.paste_collapsed);
    }

    #[test]
    fn submit_resets_collapse() {
        let long_text = "a".repeat(300);
        let mut area = InputArea::new();
        area.wrap_width = 40;

        area.insert_paste(&long_text);
        assert!(area.paste_collapsed);

        let result = area.submit();
        assert_eq!(result, long_text);
        assert!(!area.paste_collapsed);
    }
}
