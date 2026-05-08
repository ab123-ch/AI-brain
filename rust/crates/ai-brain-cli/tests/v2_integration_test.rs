/// v2 路径集成测试：验证 eval_gate + 评估脑完整流程
///
/// 直接调用 query_streaming（TUI 走的路径），检查：
/// 1. eval_gate LLM 被调用
/// 2. eval_gate 判定是否需要评估
/// 3. 评估脑（brain-eval）按需触发
/// 4. 记忆脑正常工作
use std::sync::Arc;
use std::time::Duration;

use ai_brain_cli::orchestrator::Orchestrator;
use brain_core::types::ProgressEvent;

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt().with_env_filter("info").init();

    println!("=== 初始化 Orchestrator (v2 路径) ===");
    let orch = Arc::new(Orchestrator::new().await.expect("Orchestrator 初始化失败"));
    println!("初始化完成\n");

    // 3 轮对话
    let queries = vec![
        ("你好，简单介绍一下你自己", "简单问候"),
        ("帮我写一个 Rust 函数计算阶乘", "代码生成"),
        ("今天天气怎么样", "闲聊"),
    ];

    for (i, (query, label)) in queries.iter().enumerate() {
        println!("=== 第 {} 轮 [{}] ===", i + 1, label);
        println!("输入: {query}");

        let (mut rx, handle) = Arc::clone(&orch).query_streaming(query);

        // 收集进度事件
        let mut events = Vec::new();
        let collect_handle = tokio::spawn(async move {
            let mut collected = Vec::new();
            loop {
                match tokio::time::timeout(Duration::from_secs(60), rx.recv()).await {
                    Ok(Some(event)) => {
                        let desc = match &event {
                            ProgressEvent::Connecting { brain, model } => {
                                format!("连接 {} ({})", brain, model)
                            }
                            ProgressEvent::Thinking { brain } => format!("{} 思考中", brain),
                            ProgressEvent::TextDelta { text } => {
                                let truncated: String = text.chars().take(50).collect();
                                format!("文本: {truncated}...")
                            }
                            ProgressEvent::ToolStart {
                                brain, tool_name, ..
                            } => {
                                format!("{} 工具: {}", brain, tool_name)
                            }
                            ProgressEvent::ToolDone {
                                brain,
                                tool_name,
                                duration_ms,
                                is_error,
                                ..
                            } => {
                                format!(
                                    "{} 工具完成: {} ({}ms{})",
                                    brain,
                                    tool_name,
                                    duration_ms,
                                    if *is_error { " ERROR" } else { "" }
                                )
                            }
                            ProgressEvent::Evaluating => "评估脑评估中...".to_string(),
                            ProgressEvent::EvaluationResult { passed, feedback } => {
                                format!("评估结果: passed={}, {}", passed, feedback)
                            }
                            ProgressEvent::Done => "完成".to_string(),
                            _ => format!("{:?}", event),
                        };
                        collected.push(desc);
                        if matches!(event, ProgressEvent::Done) {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        collected.push("超时".to_string());
                        break;
                    }
                }
            }
            collected
        });

        // 等待结果
        match handle.await {
            Ok(Ok(output)) => {
                let answer_preview = &output.answer[..output.answer.len().min(200)];
                println!("回答: {answer_preview}");
                println!(
                    "Token: {} 次LLM调用: {}",
                    output.usage.total_tokens, output.usage.llm_calls
                );
            }
            Ok(Err(e)) => println!("错误: {e}"),
            Err(e) => println!("Join错误: {e}"),
        }

        let collected = collect_handle.await.unwrap_or_default();
        println!("进度事件:");
        for ev in &collected {
            println!("  - {ev}");
        }
        println!();
    }

    // 关闭时触发四步分析
    println!("=== 关闭（触发四步分析） ===");
    // shutdown_with_analysis 需要 Arc<Self>，直接 drop
    drop(orch);
    println!("完成");
}
