mod api_server;
mod init;
mod orchestrator;
mod real_tool_executor;
mod repl;
mod tui;

use clap::{Parser, Subcommand};
use orchestrator::{format_output, Orchestrator};
use std::io::IsTerminal;

/// AI Brain — 多副脑并行智能系统
#[derive(Parser)]
#[command(name = "ai-brain", version, about = "AI Brain 多副脑并行智能系统")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 单次查询
    Query { query: String },
    /// 查看系统状态
    Status,
    /// 查看副脑权重
    Weights,
    /// 记忆管理
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// 副脑进化管理
    Brain {
        #[command(subcommand)]
        action: BrainAction,
    },
    /// 启动 HTTP API 服务
    Serve {
        #[arg(long, default_value = "127.0.0.1:3141")]
        addr: String,
    },
    /// v2 路径集成测试（3 轮对话，验证 eval_gate + 评估脑）
    V2Test,
}

#[derive(Subcommand)]
enum MemoryAction {
    /// 查看记忆统计
    Stats,
}

#[derive(Subcommand)]
enum BrainAction {
    /// 列出所有副脑
    List,
    /// 列出可用模板
    Templates,
    /// 从模板创建新副脑
    Create { template: String },
    /// 手动休眠副脑
    Dormant { brain_id: String },
    /// 唤醒休眠副脑
    Wake { brain_id: String },
    /// 获取 LLM 创建建议
    Suggest,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let is_tui = cli.command.is_none() && std::io::stdin().is_terminal();

    // 日志初始化：TUI 模式只写文件，避免污染 alternate screen
    let (base_dir, is_first_run) = init::init_environment();
    if is_tui {
        init::init_tui_logging(&base_dir);
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .init();
        init::init_file_logging(&base_dir);
    }
    if is_first_run && !is_tui {
        init::print_first_run_guide();
    }
    tracing::info!("AI Brain 启动，根目录: {:?}", base_dir);

    run_command(cli).await;
}

async fn run_command(cli: Cli) {
    match &cli.command {
        None => {
            let orch = Orchestrator::new().await.expect("初始化失败");
            // 使用 TUI 模式（修复旧 REPL 的 UTF-8 崩溃）
            if std::io::stdin().is_terminal() {
                tui::run(orch).await;
            } else {
                repl::run(orch).await;
            }
        }
        Some(Commands::Query { query }) => {
            let orch = Orchestrator::new().await.expect("初始化失败");
            let output = orch.query(query).await.expect("查询失败");
            println!("{}", format_output(&output));
        }
        Some(Commands::Status) => {
            let orch = Orchestrator::new().await.expect("初始化失败");
            println!("{}", orch.status());
        }
        Some(Commands::Weights) => {
            let orch = Orchestrator::new().await.expect("初始化失败");
            println!("{}", orch.weights().await);
        }
        Some(Commands::Memory { action }) => {
            let orch = Orchestrator::new().await.expect("初始化失败");
            match action {
                MemoryAction::Stats => println!("{}", orch.memory_stats().await),
            }
        }
        Some(Commands::Serve { addr }) => {
            let orch = Orchestrator::new().await.expect("初始化失败");
            api_server::serve(orch, addr).await;
        }
        Some(Commands::Brain { action }) => {
            let orch = Orchestrator::new().await.expect("初始化失败");
            handle_brain_command(orch, action).await;
        }
        Some(Commands::V2Test) => {
            use brain_core::types::ProgressEvent;
            use std::sync::Arc;
            use std::time::Duration;

            println!("=== v2 路径集成测试 ===");
            let orch: Arc<Orchestrator> = Arc::new(Orchestrator::new().await.expect("初始化失败"));

            let queries = vec![
                "你好，简单介绍一下你自己",
                "帮我写一个 Rust 函数计算阶乘，只要函数签名和实现",
                "今天天气怎么样",
            ];

            for (i, query) in queries.iter().enumerate() {
                println!("\n=== 第 {} 轮 ===", i + 1);
                println!("输入: {query}");

                let (rx, handle) = Arc::clone(&orch).query_streaming(query);
                let mut rx = rx;

                // 在后台收集进度事件
                let collect = tokio::spawn(async move {
                    let mut events: Vec<String> = Vec::new();
                    loop {
                        match tokio::time::timeout(Duration::from_secs(120), rx.recv()).await {
                            Ok(Some(event)) => {
                                let desc = match &event {
                                    ProgressEvent::Connecting { brain, model } => {
                                        format!("连接 {} ({})", brain, model)
                                    }
                                    ProgressEvent::ToolStart {
                                        brain, tool_name, ..
                                    } => format!("{} 工具: {}", brain, tool_name),
                                    ProgressEvent::ToolDone {
                                        brain,
                                        tool_name,
                                        duration_ms,
                                        is_error,
                                        ..
                                    } => format!(
                                        "{} 完成: {} ({}ms{})",
                                        brain,
                                        tool_name,
                                        duration_ms,
                                        if *is_error { " ERR" } else { "" }
                                    ),
                                    ProgressEvent::Evaluating => "评估脑评估中".into(),
                                    ProgressEvent::EvaluationResult { passed, feedback } => {
                                        format!("评估结果: passed={}, {}", passed, feedback)
                                    }
                                    ProgressEvent::Done => "完成".into(),
                                    _ => format!("{:?}", event),
                                };
                                events.push(desc);
                                if matches!(event, ProgressEvent::Done) {
                                    break;
                                }
                            }
                            Ok(None) => break,
                            Err(_) => {
                                events.push("超时".into());
                                break;
                            }
                        }
                    }
                    events
                });

                match handle.await {
                    Ok(Ok(output)) => {
                        let p: String = output.answer.chars().take(200).collect();
                        println!("回答: {p}");
                    }
                    Ok(Err(e)) => println!("错误: {e}"),
                    Err(e) => println!("Join错误: {e}"),
                }

                if let Ok(events) = collect.await {
                    println!("进度事件:");
                    for ev in &events {
                        println!("  - {ev}");
                    }
                }
            }

            println!("\n=== 测试完成，关闭 ===");
            orch.shutdown_with_analysis().await;
            println!("记忆已保存");
        }
    }
}

async fn handle_brain_command(orch: Orchestrator, action: &BrainAction) {
    match action {
        BrainAction::List => print!("{}", orch.brain_status_text().await),
        BrainAction::Templates => {
            let templates = orch.list_templates().await;
            if templates.is_empty() {
                println!("没有可用模板。");
            } else {
                println!("=== 可用模板 ===");
                for t in &templates {
                    println!("  {} — {}", t.name, t.description);
                    if !t.capabilities.is_empty() {
                        println!("    能力: {:?}", t.capabilities);
                    }
                }
            }
        }
        BrainAction::Create { template } => match orch.create_brain(template).await {
            Ok(id) => println!("副脑 {id} 创建成功！"),
            Err(e) => eprintln!("创建失败: {e}"),
        },
        BrainAction::Dormant { brain_id } => {
            let id = brain_core::types::BrainId(brain_id.clone());
            match orch.dormant_brain(&id).await {
                Ok(()) => println!("副脑 {brain_id} 已休眠。"),
                Err(e) => eprintln!("休眠失败: {e}"),
            }
        }
        BrainAction::Wake { brain_id } => {
            let id = brain_core::types::BrainId(brain_id.clone());
            match orch.wake_brain(&id).await {
                Ok(weight) => println!("副脑 {brain_id} 已唤醒，权重: {weight:.2}"),
                Err(e) => eprintln!("唤醒失败: {e}"),
            }
        }
        BrainAction::Suggest => print!("{}", orch.suggestions_text().await),
    }
}
