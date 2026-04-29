mod app;
mod input;
mod output;
mod session_logger;
mod status;

pub use app::App;

use crate::orchestrator::Orchestrator;

/// 启动 TUI 模式
pub async fn run(orch: Orchestrator) {
    let mut app = App::new(orch);

    // 1. 启用 raw mode（必须！否则键盘事件被行缓冲，无法实时捕获）
    crossterm::terminal::enable_raw_mode().expect("无法启用 raw mode");

    // 2. 启用 bracketed paste & 进入 alternate screen（一并 flush）
    use std::io::Write as _;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
    )
    .expect("无法进入 alternate screen");
    // bracketed paste 需要独立的写入 + flush（crossterm 没有内置命令）
    write!(std::io::stdout(), "\x1b[?2004h").ok();
    std::io::stdout().flush().ok();

    // 3. 创建后端
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))
            .expect("无法初始化终端");

    // 3. 运行主循环
    let result = app.run(&mut terminal).await;

    // 4. 清理恢复（stdout() 返回新 handle，不与 terminal 冲突）
    // 先禁用 bracketed paste（flush 确保生效）
    write!(std::io::stdout(), "\x1b[?2004l").ok();
    std::io::stdout().flush().ok();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    )
    .ok();
    terminal.show_cursor().ok();
    crossterm::terminal::disable_raw_mode().ok();

    if let Err(e) = result {
        tracing::error!("TUI 运行错误: {e}");
    }
}
