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
            cmd if cmd == ":evo" || cmd.starts_with(":evo ") => {
                let goal = input.strip_prefix(":evo").unwrap_or("").trim();
                if goal.is_empty() {
                    println!("用法: :evo <目标描述>");
                } else {
                    match orch.spawn_evolution(Some(goal.to_string())).await {
                        Ok(()) => println!("进化已启动（后台运行），使用 :evo-status 查看"),
                        Err(e) => eprintln!("启动失败: {e}"),
                    }
                }
                continue;
            }
            ":evo-status" => {
                println!("{}", orch.evo_status_v2().await);
                continue;
            }
            ":evo-approve" | ":evo-reject" | ":evo-diff" => {
                println!("v1 引擎已移除，请使用 :evo <目标> 启动 v2 进化循环");
                continue;
            }
            ":quit" | ":exit" | "exit" | "quit" => {
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

    println!("正在保存记忆（触发四步分析）...");
    orch.shutdown_with_analysis().await;
    println!("记忆已保存。再见！");
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
    println!("  :evo <目标> — 启动进化任务");
    println!("  :evo-status — 查看进化状态");
    println!("  :evo-approve — 确认合并进化结果");
    println!("  :evo-reject  — 拒绝并回滚进化");
    println!("  :evo-diff    — 查看进化变更");
    println!("  :quit      — 退出");
    println!("  其他输入   — 发送查询");
    println!();
}
