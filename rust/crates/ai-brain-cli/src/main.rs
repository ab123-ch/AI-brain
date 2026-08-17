use ai_brain_cli::{api_server, init, orchestrator, remote_access, repl, tui};
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
    /// 启动 Web UI
    Web {
        /// 覆盖 [remote_access].port；自动远程访问仅用于回环监听地址
        #[arg(long)]
        addr: Option<String>,
    },
    /// 通过 Tailscale 私有网络启动远程 Web UI
    Remote {
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
    /// v2 路径集成测试（3 轮对话，验证 eval_gate + 评估脑）
    V2Test,
    /// 导出完整 system prompt 到桌面文件（调试用）
    DumpSystemPrompt,
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

    // 日志初始化：TUI 模式只写文件，其他模式同时写终端和文件。
    let (base_dir, is_first_run) = init::init_environment();
    if let Err(error) = init::init_logging(&base_dir, is_tui) {
        eprintln!("初始化日志失败: {error}");
        std::process::exit(1);
    }
    // 首次运行：总是在终端显示引导（TUI 模式也不例外，此时尚未进入 alternate screen）
    if is_first_run {
        init::print_first_run_guide();
        // 首次运行没有配置 API key，LLM 初始化必然失败，直接退出
        std::process::exit(0);
    }

    tracing::info!("AI Brain 启动，根目录: {:?}", base_dir);

    // 每次启动时自动导出完整 system prompt 到桌面
    dump_system_prompt_to_desktop();

    run_command(cli).await;
}

/// 导出完整 system prompt 到桌面文件
fn dump_system_prompt_to_desktop() {
    use brain_main::prompts;
    let prompt = prompts::build_full_system_prompt(None);
    let desktop =
        dirs::desktop_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let path = desktop.join("系统提示词.txt");
    match std::fs::write(&path, &prompt) {
        Ok(()) => tracing::info!(
            "系统提示词已导出到: {} ({} tokens)",
            path.display(),
            prompt.chars().count() * 3 / 4
        ),
        Err(e) => tracing::warn!("系统提示词导出失败: {e}"),
    }
}

/// 初始化编排器，失败时打印错误并退出
async fn init_or_die() -> Orchestrator {
    match Orchestrator::new().await {
        Ok(orch) => orch,
        Err(e) => {
            eprintln!("初始化失败: {e}");
            std::process::exit(1);
        }
    }
}

async fn run_command(cli: Cli) {
    match &cli.command {
        None => {
            let orch = init_or_die().await;
            // 使用 TUI 模式（修复旧 REPL 的 UTF-8 崩溃）
            if std::io::stdin().is_terminal() {
                tui::run(orch).await;
            } else {
                repl::run(orch).await;
            }
        }
        Some(Commands::Query { query }) => {
            let orch = init_or_die().await;
            match orch.query(query).await {
                Ok(output) => println!("{}", format_output(&output)),
                Err(e) => eprintln!("查询失败: {e}"),
            }
        }
        Some(Commands::Status) => match Orchestrator::new().await {
            Ok(orch) => println!("{}", orch.status()),
            Err(e) => println!("=== AI Brain 系统状态 ===\n  LLM 不可用: {e}"),
        },
        Some(Commands::Weights) => {
            let orch = init_or_die().await;
            println!("{}", orch.weights().await);
        }
        Some(Commands::Memory { action }) => {
            let orch = init_or_die().await;
            match action {
                MemoryAction::Stats => println!("{}", orch.memory_stats().await),
            }
        }
        Some(Commands::Serve { addr }) => {
            let orch = init_or_die().await;
            api_server::serve(orch, addr).await;
        }
        Some(Commands::Web { addr }) => {
            let config_path = init::base_dir().join("config.toml");
            let settings = match remote_access::RemoteAccessSettings::load(&config_path) {
                Ok(settings) => Some(settings),
                Err(error) => {
                    eprintln!("远程访问配置读取失败，将使用本地模式: {error}");
                    None
                }
            };
            let addr = addr.clone().unwrap_or_else(|| {
                format!(
                    "127.0.0.1:{}",
                    settings
                        .as_ref()
                        .map_or(remote_access::DEFAULT_REMOTE_PORT, |settings| settings
                            .port())
                )
            });
            let endpoint = settings.as_ref().and_then(|settings| {
                prepare_automatic_remote_access(settings, &config_path, &addr)
            });
            let orch = init_or_die().await;
            let result = if remote_access::loopback_port(&addr).is_some() {
                api_server::serve_web_auto_remote(
                    orch,
                    &addr,
                    endpoint.as_ref().map(remote_access::RemoteEndpoint::host),
                )
                .await
            } else {
                api_server::serve_web(orch, &addr).await
            };
            if let Err(error) = result {
                eprintln!("Web UI 启动失败: {error}");
                std::process::exit(1);
            }
        }
        Some(Commands::Remote { port }) => match remote_access::configure(*port) {
            Ok(endpoint) => {
                let config_path = init::base_dir().join("config.toml");
                if let Err(error) = remote_access::persist_endpoint(&config_path, &endpoint, *port)
                {
                    eprintln!("固定远程地址写入配置失败，但本次远程访问仍可使用: {error}");
                }
                println!("\n=== 智脑安全远程模式 ===");
                println!("手机 / 异地电脑访问: {}", endpoint.url());
                println!("访问范围: 仅当前 Tailscale 私有网络");
                println!("本机监听: http://127.0.0.1:{port}");
                println!("配置文件: {}", config_path.display());
                println!("停止共享: tailscale serve off\n");

                let orch = init_or_die().await;
                let addr = format!("127.0.0.1:{port}");
                if let Err(error) = api_server::serve_web_remote(orch, &addr, endpoint.host()).await
                {
                    eprintln!("远程 Web UI 启动失败: {error}");
                    std::process::exit(1);
                }
            }
            Err(error) => {
                eprintln!("远程模式启动失败: {error}");
                std::process::exit(1);
            }
        },
        Some(Commands::Brain { action }) => {
            let orch = init_or_die().await;
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

                let (rx, handle, _cancel) = Arc::clone(&orch).query_streaming(query);
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
        Some(Commands::DumpSystemPrompt) => {
            dump_system_prompt_to_desktop();
            let desktop =
                dirs::desktop_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            println!(
                "系统提示词已导出到: {}",
                desktop.join("系统提示词.txt").display()
            );
        }
    }
}

fn prepare_automatic_remote_access(
    settings: &remote_access::RemoteAccessSettings,
    config_path: &std::path::Path,
    addr: &str,
) -> Option<remote_access::RemoteEndpoint> {
    if !settings.enabled() {
        tracing::info!("Tailscale 自动远程访问已由配置关闭");
        return None;
    }

    let Some(port) = remote_access::loopback_port(addr) else {
        eprintln!("自动远程访问未启用：Web 监听地址 {addr} 不是回环地址；请使用 127.0.0.1:<端口>");
        return None;
    };

    match remote_access::configure(port) {
        Ok(endpoint) => {
            if let Err(error) = remote_access::persist_endpoint(config_path, &endpoint, port) {
                eprintln!("固定远程地址写入配置失败，但本次远程访问仍可使用: {error}");
            }
            println!("\n=== 智脑私有远程访问已自动启动 ===");
            println!("固定地址: {}", endpoint.url());
            println!("本机地址: http://{addr}");
            println!("配置文件: {}\n", config_path.display());
            Some(endpoint)
        }
        Err(error) => {
            if port == settings.port() {
                if let Some(endpoint) = settings.cached_endpoint() {
                    eprintln!("Tailscale 当前尚未就绪: {error}");
                    eprintln!(
                        "将保留固定入口 {}；已有 Serve 配置会随 Tailscale 服务恢复。",
                        endpoint.url()
                    );
                    return Some(endpoint);
                }
            }
            eprintln!("Tailscale 自动远程访问暂不可用，本机 Web 仍会启动: {error}");
            None
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
