//! 输入区域组件
//!
//! 封装 tui-textarea 多行编辑器 + 粘贴处理。

use ratatui::style::{Color, Style};
use ratatui::widgets::block::Position;
use ratatui::widgets::Block;
use tui_textarea::TextArea;

/// 输入区域组件
pub struct InputArea {
    /// tui-textarea 编辑器
    pub textarea: TextArea<'static>,
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

        Self { textarea }
    }

    /// 获取当前输入内容
    pub fn input_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// 清空输入
    pub fn clear(&mut self) {
        self.textarea.select_all();
        self.textarea.cut();
    }

    /// 提交当前输入
    pub fn submit(&mut self) -> String {
        let text = self.input_text();
        self.clear();
        text
    }

    /// 处理粘贴事件
    pub fn insert_paste(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\n' || ch == '\r' {
                self.textarea.insert_char(' ');
            } else {
                self.textarea.insert_char(ch);
            }
        }
    }
}

impl Default for InputArea {
    fn default() -> Self {
        Self::new()
    }
}
