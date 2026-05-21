mod app;
mod command_panel;
mod completion;
mod input;
mod output;
mod session_logger;
mod status;

pub use app::App;
pub use completion::{
    CompletionCategory, CompletionItem, CompletionPopup, EvolutionCompleter, InputContext,
};

use crate::orchestrator::Orchestrator;

/// 启动 TUI 模式
pub async fn run(orch: Orchestrator) {
    let mut app = App::new(orch);

    // 1. 启用 raw mode（必须！否则键盘事件被行缓冲，无法实时捕获）
    crossterm::terminal::enable_raw_mode().expect("无法启用 raw mode");

    // 2. 启用 bracketed paste & 进入 alternate screen（一并 flush）
    use std::io::Write as _;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen,)
        .expect("无法进入 alternate screen");
    // 手动启用鼠标捕获：只用 normal tracking (?1000h) + SGR 模式 (?1006h)
    // 不启用 button-event (?1002h) 和 any-event (?1003h)，
    // 这样左键拖拽不被捕获，终端原生文本选择可以正常工作。
    write!(std::io::stdout(), "\x1b[?1000h\x1b[?1006h").ok();
    // bracketed paste
    write!(std::io::stdout(), "\x1b[?2004h").ok();
    std::io::stdout().flush().ok();

    // 3. 创建后端
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))
            .expect("无法初始化终端");

    // 3. 运行主循环
    let result = app.run(&mut terminal).await;

    // 4. 清理恢复
    // 禁用鼠标捕获（只关我们开的两个）
    write!(std::io::stdout(), "\x1b[?1000l\x1b[?1006l").ok();
    // 禁用 bracketed paste
    write!(std::io::stdout(), "\x1b[?2004l").ok();
    std::io::stdout().flush().ok();
    crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    crossterm::terminal::disable_raw_mode().ok();

    if let Err(e) = result {
        tracing::error!("TUI 运行错误: {e}");
    }
}
