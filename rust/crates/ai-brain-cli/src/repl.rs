use std::io::{self, BufRead, Write};

use crate::orchestrator::{format_output, Orchestrator};

/// 交互式 REPL 循环
pub async fn run(orch: Orchestrator) {
    println!("╔══════════════════════════════════╗");
    println!("║     AI Brain 多副脑智能系统      ║");
    println!("║     输入查询，或 :help 查看命令   ║");
    println!("╚══════════════════════════════════╝");
    println!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("读取输入失败: {e}");
                break;
            }
        };

        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }

        // 内置命令
        match input.as_str() {
            ":help" | "help" => {
                print_help();
                continue;
            }
            ":status" | "status" => {
                println!("{}", orch.status());
                continue;
            }
            ":weights" => {
                println!("{}", orch.weights().await);
                continue;
            }
            ":memory" => {
                println!("{}", orch.memory_stats().await);
                continue;
            }
            ":evaluate" => {
                println!("{}", orch.evaluate_default());
                continue;
            }
            ":brains" => {
                print!("{}", orch.brain_status_text().await);
                continue;
            }
            ":suggest" => {
                print!("{}", orch.suggestions_text().await);
                continue;
            }
            ":templates" => {
                let templates = orch.list_templates().await;
                if templates.is_empty() {
                    println!("没有可用模板。");
                } else {
                    println!("=== 可用模板 ===");
                    for t in &templates {
                        println!("  {} — {}", t.name, t.description);
                    }
                }
                println!();
                continue;
            }
            ":quit" | ":exit" | "exit" | "quit" => {
                println!("再见！");
                break;
            }
            _ => {}
        }

        // 查询
        print!("思考中... ");
        let _ = stdout.flush();
        match orch.query(&input).await {
            Ok(output) => {
                print!("\r");
                println!("{}", format_output(&output));
            }
            Err(e) => {
                print!("\r");
                eprintln!("错误: {e}");
            }
        }
        println!();
    }

    orch.shutdown();
}

fn print_help() {
    println!("=== AI Brain 命令 ===");
    println!("  :help      — 显示帮助");
    println!("  :status    — 系统状态");
    println!("  :weights   — 副脑权重");
    println!("  :memory    — 记忆统计");
    println!("  :evaluate  — 手动评估");
    println!("  :brains    — 副脑列表（活跃+休眠）");
    println!("  :templates — 可用副脑模板");
    println!("  :suggest   — 创建建议");
    println!("  :quit      — 退出");
    println!("  其他输入   — 发送查询");
    println!();
}
