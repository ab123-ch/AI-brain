//! 状态栏组件
//!
//! 显示模型名、上下文使用率、当前轮次等信息。

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::orchestrator::SystemStatus;

/// 状态栏数据
pub struct StatusBar {
    /// 模型名
    pub model: String,
    /// 上下文使用率 (0.0~1.0)
    pub context_usage: f32,
    /// 当前轮次
    pub round: u32,
    /// 评估是否启用
    pub eval_enabled: bool,
    /// 是否正在处理（显示 spinner）
    pub busy: bool,
}

impl StatusBar {
    pub fn from_system_status(status: &SystemStatus) -> Self {
        Self {
            model: status.model.clone(),
            context_usage: 0.0,
            round: status.query_count,
            eval_enabled: status.eval_enabled,
            busy: false,
        }
    }

    /// 渲染状态栏到指定区域
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let ctx_pct = (self.context_usage * 100.0) as u32;
        let _ctx_color = if ctx_pct > 80 {
            Color::Red
        } else if ctx_pct > 50 {
            Color::Yellow
        } else {
            Color::Green
        };

        let eval_str = if self.eval_enabled {
            "评估:开"
        } else {
            "评估:关"
        };
        let busy_indicator = if self.busy { " *" } else { "" };

        let text = format!(
            " {} | 上下文 {}% | 第{}轮 | {}{} ",
            self.model, ctx_pct, self.round, eval_str, busy_indicator
        );

        let paragraph = Paragraph::new(text).style(
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

        f.render_widget(paragraph, area);
    }
}
