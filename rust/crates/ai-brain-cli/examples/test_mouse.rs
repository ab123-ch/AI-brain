//! 鼠标捕获模式测试程序
//!
//! 用法: cargo run --example test_mouse -- [mode]
//!   mode: "full" (crossterm默认), "normal" (1000+1006), "altscroll" (1007+1006)
//!
//! 测试方法: 运行后，尝试滚轮滚动和左键拖选文本，观察日志

use std::io::{self, Read as _, Write as _};
use std::time::Duration;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "normal".into());
    let log_path = "/tmp/tui_mouse_test.log";

    let mut log = std::fs::File::create(log_path).expect("无法创建日志文件");

    // 进入 raw mode + alternate screen
    crossterm::terminal::enable_raw_mode().expect("无法启用 raw mode");
    crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen)
        .expect("无法进入 alternate screen");

    let enable_seq = match mode.as_str() {
        "full" => {
            // crossterm 默认: 1000+1002+1003+1015+1006
            "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1015h\x1b[?1006h".to_string()
        }
        "normal" => {
            // 只有 normal tracking + SGR
            "\x1b[?1000h\x1b[?1006h".to_string()
        }
        "altscroll" => {
            // alternate scroll + SGR (不捕获按钮)
            "\x1b[?1007h\x1b[?1006h".to_string()
        }
        "normal_altscroll" => {
            // normal tracking + alternate scroll + SGR
            "\x1b[?1000h\x1b[?1007h\x1b[?1006h".to_string()
        }
        _ => {
            eprintln!("Unknown mode: {mode}. Use: full, normal, altscroll, normal_altscroll");
            crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen).ok();
            crossterm::terminal::disable_raw_mode().ok();
            return;
        }
    };

    writeln!(log, "=== Mouse test mode: {mode} ===").ok();
    writeln!(log, "Enable sequence: {:?}", enable_seq).ok();
    writeln!(log, "Instructions: scroll wheel, click, drag, press Esc to quit").ok();
    writeln!(log, "---").ok();

    write!(io::stdout(), "{enable_seq}").ok();
    // 显示提示
    write!(
        io::stdout(),
        "\r\nMouse test mode: {mode}\r\nScroll, click, drag. Press Esc to quit.\r\n"
    )
    .ok();
    io::stdout().flush().ok();

    let mut event_count = 0u32;
    let start = std::time::Instant::now();

    loop {
        if crossterm::event::poll(Duration::from_millis(100)).unwrap() {
            match crossterm::event::read().unwrap() {
                crossterm::event::Event::Mouse(m) => {
                    event_count += 1;
                    let kind_str = format!("{:?}", m.kind);
                    writeln!(
                        log,
                        "[{:.3}s] MOUSE {:?} row={} col={} modifiers={:?}",
                        start.elapsed().as_secs_f64(),
                        m.kind,
                        m.row,
                        m.column,
                        m.modifiers
                    )
                    .ok();
                    write!(io::stdout(), "\r[#{event_count}] {kind_str} ({},{})  ", m.row, m.column)
                        .ok();
                    io::stdout().flush().ok();
                }
                crossterm::event::Event::Key(k) => {
                    event_count += 1;
                    writeln!(
                        log,
                        "[{:.3}s] KEY {:?} modifiers={:?}",
                        start.elapsed().as_secs_f64(),
                        k.code,
                        k.modifiers
                    )
                    .ok();
                    if k.code == crossterm::event::KeyCode::Esc {
                        break;
                    }
                    // 检查是否收到滚轮被转成的方向键
                    if matches!(k.code, crossterm::event::KeyCode::Up | crossterm::event::KeyCode::Down)
                        && !k.modifiers.contains(crossterm::event::KeyModifiers::SHIFT)
                    {
                        write!(io::stdout(), "\r[#{event_count}] KEY({:?}) - 可能是滚轮!  ", k.code).ok();
                        io::stdout().flush().ok();
                    }
                }
                crossterm::event::Event::Resize(w, h) => {
                    writeln!(log, "[{:.3}s] RESIZE {}x{}", start.elapsed().as_secs_f64(), w, h).ok();
                }
                _ => {}
            }
        }

        // 15 秒后自动退出
        if start.elapsed() > Duration::from_secs(15) {
            writeln!(log, "--- Timeout after 15s, events: {event_count} ---").ok();
            break;
        }
    }

    // 清理
    let disable_seq = match mode.as_str() {
        "full" => "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l",
        "normal" => "\x1b[?1000l\x1b[?1006l",
        "altscroll" => "\x1b[?1007l\x1b[?1006l",
        "normal_altscroll" => "\x1b[?1000l\x1b[?1007l\x1b[?1006l",
        _ => "",
    };
    write!(io::stdout(), "{disable_seq}").ok();
    io::stdout().flush().ok();

    crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen).ok();
    crossterm::terminal::disable_raw_mode().ok();

    writeln!(log, "=== Test complete. Total events: {event_count} ===").ok();
    eprintln!("\nLog written to {log_path}");
}
