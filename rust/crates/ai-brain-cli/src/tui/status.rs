//! 状态栏组件
//!
//! 显示模型名、上下文使用率、token 消耗、预估费用、缓存命中率、当前轮次等信息。

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::orchestrator::SystemStatus;

/// 状态栏数据
#[derive(Default)]
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
    /// 会话累计 prompt tokens
    pub cumulative_prompt_tokens: u64,
    /// 会话累计 completion tokens
    pub cumulative_completion_tokens: u64,
    /// 会话累计 cache read tokens
    pub cumulative_cache_read_tokens: u64,
}

impl StatusBar {
    pub fn from_system_status(status: &SystemStatus) -> Self {
        Self {
            model: status.model.clone(),
            context_usage: status.context_usage as f32,
            round: status.query_count,
            eval_enabled: status.eval_enabled,
            busy: false,
            cumulative_prompt_tokens: status.cumulative_prompt_tokens,
            cumulative_completion_tokens: status.cumulative_completion_tokens,
            cumulative_cache_read_tokens: status.cumulative_cache_read_tokens,
        }
    }

    /// 渲染状态栏到指定区域
    ///
    /// 上下文使用率 <1.0% 时显示一位小数（如 0.5%），>=1.0% 时显示整数。
    /// Token 计数用 k 简写（如 8k入/1.2k出），费用根据模型名估算。
    /// 缓存命中率在有缓存命中时显示。
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let ctx_pct_f = self.context_usage * 100.0;
        let ctx_display = if ctx_pct_f > 100.0 {
            // 超过 100% 时显示实际值并添加警告
            format!("{:.0}⚠", ctx_pct_f)
        } else if ctx_pct_f < 1.0 {
            format!("{:.1}", ctx_pct_f)
        } else {
            format!("{}", ctx_pct_f as u32)
        };
        let ctx_color = if ctx_pct_f > 100.0 {
            Color::Red
        } else if ctx_pct_f > 80.0 {
            Color::Red
        } else if ctx_pct_f > 50.0 {
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

        // Token 计数 + 缓存命中率 + 费用估算
        let total_input = self.cumulative_prompt_tokens + self.cumulative_cache_read_tokens;
        let tok_display = if self.cumulative_prompt_tokens > 0
            || self.cumulative_completion_tokens > 0
        {
            let in_tok = short_tokens(self.cumulative_prompt_tokens);
            let out_tok = short_tokens(self.cumulative_completion_tokens);
            let cost = estimate_cost(
                &self.model,
                self.cumulative_prompt_tokens,
                self.cumulative_completion_tokens,
            );

            // 缓存命中率
            let cache_str = if self.cumulative_cache_read_tokens > 0 && total_input > 0 {
                let hit_pct = self.cumulative_cache_read_tokens as f64 / total_input as f64 * 100.0;
                let cache_tok = short_tokens(self.cumulative_cache_read_tokens);
                format!("缓存:{hit_pct:.0}%({cache_tok}) ")
            } else {
                String::new()
            };

            format!("Tok: {in_tok}入/{out_tok}出 {cache_str}{cost}")
        } else {
            String::new()
        };

        let text = format!(
            " {} | 上下文 {}%{} | 第{}轮 | {}{} ",
            self.model,
            ctx_display,
            if tok_display.is_empty() {
                String::new()
            } else {
                format!(" | {tok_display}")
            },
            self.round,
            eval_str,
            busy_indicator
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

/// Token 计数简写：>=1000 用 k，否则直接用数字
fn short_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

/// 根据模型名和累计 tokens 估算费用（USD）
fn estimate_cost(model: &str, prompt_tokens: u64, completion_tokens: u64) -> String {
    let (input_price, output_price) = pricing_for_model(model);
    #[allow(clippy::cast_precision_loss)]
    let cost = (prompt_tokens as f64) / 1_000_000.0 * input_price
        + (completion_tokens as f64) / 1_000_000.0 * output_price;
    if cost < 0.0001 {
        "<$0.0001".to_string()
    } else if cost < 0.01 {
        format!("${:.4}", cost)
    } else {
        format!("${:.2}", cost)
    }
}

/// 根据模型名返回 (input_price_per_1M, output_price_per_1M)
fn pricing_for_model(model: &str) -> (f64, f64) {
    let normalized = model.to_ascii_lowercase();
    // DeepSeek 定价（约）
    if normalized.contains("deepseek") {
        if normalized.contains("r1") {
            return (0.55, 2.19); // deepseek-r1
        }
        return (0.27, 1.10); // deepseek-v3 / deepseek-chat
    }
    // GLM 定价（约）
    if normalized.contains("glm") {
        if normalized.contains("4.7") || normalized.contains("4-flash") {
            return (0.10, 0.10); // GLM-4-Flash 免费/极低价
        }
        return (0.10, 0.10); // GLM 默认
    }
    // Claude 定价
    if normalized.contains("sonnet") {
        return (3.0, 15.0);
    }
    if normalized.contains("haiku") {
        return (0.80, 4.0);
    }
    if normalized.contains("opus") {
        return (15.0, 75.0);
    }
    // GPT 定价
    if normalized.contains("gpt-4o") {
        return (2.50, 10.0);
    }
    if normalized.contains("gpt-4") {
        return (30.0, 60.0);
    }
    if normalized.contains("gpt-3.5") {
        return (0.50, 1.50);
    }
    // 默认：Sonnet 级别估
    (3.0, 15.0)
}
