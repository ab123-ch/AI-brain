//! 真实 LLM 端到端测试
//!
//! 所有测试标记为 #[ignore]，通过 `cargo test -- --ignored` 运行。
//! 需要配置 ZHIPU_API_KEY 环境变量。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use brain_core::agent::BrainAgent;
use brain_llm::{ChatMessage, ChatRequest, LlmConfig, LlmProvider};

// ─── 辅助 ────────────────────────────────────────────────────────

/// 适配 brain-llm LlmProvider → brain-sensory LlmProvider
struct LlmAdapter {
    inner: Box<dyn brain_llm::LlmProvider>,
}

impl brain_sensory::llm::LlmProvider for LlmAdapter {
    fn complete(
        &self,
        model: &str,
        system_prompt: &str,
        user_input: &str,
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        let request = ChatRequest {
            model: Some(model.into()),
            messages: vec![
                ChatMessage::system(system_prompt),
                ChatMessage::user(user_input),
            ],
            max_tokens: Some(max_tokens),
            temperature: None,
            stream: None,
        };
        Box::pin(async move {
            self.inner
                .complete(request)
                .await
                .map(|r| r.content)
                .map_err(|e| e.to_string())
        })
    }
}

/// 创建真实 LLM 客户端，跳过无 API Key 的情况
fn real_llm_client(brain_name: &str) -> Option<Box<dyn LlmProvider>> {
    let config = LlmConfig::load_default().ok()?;
    let api_key = config.resolve_api_key(&config.llm.default_provider).ok()?;
    if api_key.is_empty() || api_key == "your-api-key-here" {
        return None;
    }
    config.create_brain_client(brain_name).ok()
}

// ─── 测试 ────────────────────────────────────────────────────────

/// 测试 1: LLM 连通性 — 最简单的调用
#[tokio::test]
#[ignore = "需要真实 LLM API Key"]
async fn real_llm_connectivity() {
    let client = real_llm_client("sensory").expect("无法创建 LLM 客户端，请配置 ZHIPU_API_KEY");

    let request = ChatRequest {
        model: None,
        messages: vec![ChatMessage::user("你好，请用一句话回复")],
        max_tokens: Some(256),
        temperature: Some(0.3),
        stream: None,
    };

    let start = Instant::now();
    let response = client.complete(request).await.expect("LLM 调用失败");
    let elapsed = start.elapsed();

    let usage = &response.usage;
    println!("[连通性测试] 回复: {}", response.content);
    println!("[连通性测试] 模型: {}, 耗时: {:?}", response.model, elapsed);
    println!(
        "[连通性测试] Token 用量: prompt={}, completion={}, total={}",
        usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
    );

    assert!(!response.content.is_empty(), "LLM 回复不应为空");
    assert!(usage.total_tokens > 0, "应有 token 用量");
    assert!(elapsed.as_secs() < 30, "应在 30s 内返回");
}

/// 测试 2: 推理脑 LLM 深度推理
#[tokio::test]
#[ignore = "需要真实 LLM API Key"]
async fn real_llm_reasoning_brain() {
    use brain_core::types::{BrainContext, BroadcastMessage};
    use brain_reasoning::reasoning_brain::{ReasoningBrain, ReasoningConfig};
    use chrono::Utc;
    use tempfile::TempDir;

    let llm: Arc<dyn LlmProvider> =
        Arc::from(real_llm_client("reasoning").expect("无法创建推理脑 LLM 客户端"));

    let tmp = TempDir::new().unwrap();
    let mut reasoning = ReasoningBrain::new(ReasoningConfig {
        experience_path: tmp.path().join("exp.json"),
        min_success_rate: 0.5,
        fast_think_threshold: 0.7,
    })
    .unwrap();
    reasoning.set_llm(llm);
    std::mem::forget(tmp);

    let msg = BroadcastMessage {
        content: "请分析 Rust 和 Go 在并发编程上的核心区别".into(),
        raw_input: "Rust vs Go 并发".into(),
        context: BrainContext {
            current_date: "2026-04-08".into(),
            cwd: "/test".into(),
            git_branch: Some("main".into()),
            platform: "darwin".into(),
        },
        timestamp: Utc::now(),
    };

    let start = Instant::now();
    let result = reasoning
        .slow_think(
            &msg,
            &brain_core::types::ThinkContext {
                related_memories: Vec::new(),
                task_history: Vec::new(),
            },
        )
        .await;
    let elapsed = start.elapsed();

    println!("[推理脑测试] 结论: {}", result.conclusion);
    println!("[推理脑测试] 推理路径: {:?}", result.reasoning_path);
    println!(
        "[推理脑测试] 置信度: {:.2}, 耗时: {:?}",
        result.confidence, elapsed
    );

    assert!(!result.conclusion.is_empty(), "推理结论不应为空");
    assert!(result.confidence > 0.3, "置信度应 > 0.3");
    assert!(result.new_experience.is_some(), "慢思考应产出新经验");
}

/// 测试 3: 执行脑 LLM 工具选择
#[tokio::test]
#[ignore = "需要真实 LLM API Key"]
async fn real_llm_motor_brain() {
    use brain_core::types::{BrainContext, BroadcastMessage, ThinkContext};
    use brain_motor::motor_brain::{MotorBrain, MotorConfig};
    use chrono::Utc;

    let llm: Arc<dyn LlmProvider> =
        Arc::from(real_llm_client("motor").expect("无法创建执行脑 LLM 客户端"));

    let mut motor = MotorBrain::new(MotorConfig::default()).unwrap();
    motor.set_llm(llm);

    let msg = BroadcastMessage {
        content: "帮我搜索项目中所有包含 TODO 的代码文件".into(),
        raw_input: "搜索 TODO".into(),
        context: BrainContext {
            current_date: "2026-04-08".into(),
            cwd: "/test".into(),
            git_branch: Some("main".into()),
            platform: "darwin".into(),
        },
        timestamp: Utc::now(),
    };

    let start = Instant::now();
    let result = motor
        .slow_think(
            &msg,
            &ThinkContext {
                related_memories: Vec::new(),
                task_history: Vec::new(),
            },
        )
        .await;
    let elapsed = start.elapsed();

    println!("[执行脑测试] 结论: {}", result.conclusion);
    println!("[执行脑测试] 推理路径: {:?}", result.reasoning_path);
    println!(
        "[执行脑测试] 置信度: {:.2}, 耗时: {:?}",
        result.confidence, elapsed
    );

    assert!(!result.conclusion.is_empty(), "工具选择结论不应为空");
}

/// 测试 4: 感知脑 LLM 解析
#[tokio::test]
#[ignore = "需要真实 LLM API Key"]
async fn real_llm_sensory_brain() {
    use brain_bus::BrainBus;
    use brain_sensory::SensoryBrain;

    let bus = Arc::new(BrainBus::new(64, 64, 64));
    let config = LlmConfig::load_default().expect("LLM 配置加载失败");
    let client = config
        .create_brain_client("sensory")
        .expect("感知脑 LLM 客户端创建失败");

    let sensory = SensoryBrain::new(
        config.model_for_brain("sensory"),
        bus.clone(),
        Box::new(LlmAdapter { inner: client }),
    );

    let mut broadcast_rx = bus.subscribe_broadcast();

    let start = Instant::now();
    let result = sensory
        .process_input("帮我分析这段 Rust 代码的性能瓶颈")
        .await;
    let elapsed = start.elapsed();

    match result {
        Ok(parsed) => {
            println!("[感知脑测试] 解析结果: {parsed}");
            assert!(!parsed.is_empty(), "解析结果不应为空");

            let msg = broadcast_rx.recv().await.expect("应有广播消息");
            assert_eq!(msg.raw_input, "帮我分析这段 Rust 代码的性能瓶颈");
            println!("[感知脑测试] 广播内容: {}", msg.content);
            println!("[感知脑测试] 耗时: {elapsed:?}");
        }
        Err(e) => {
            panic!("感知脑处理失败: {e}");
        }
    }
}

/// 测试 5: 记忆脑存储 + 召回
#[tokio::test]
#[ignore = "需要真实 LLM API Key"]
async fn real_llm_memory_recall() {
    use brain_core::types::{BrainContext, BroadcastMessage};
    use brain_memory::memory_brain::{MemoryBrain, MemoryBrainConfig};
    use chrono::Utc;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let mut memory = MemoryBrain::new(MemoryBrainConfig {
        base_dir: tmp.path().to_path_buf(),
        session_id: "e2e-mem-test".into(),
        max_keywords: 5,
    })
    .unwrap();

    let msgs = [
        "Rust 的所有权系统避免了 GC 开销",
        "Tokio 是 Rust 最常用的异步运行时",
        "Go 的 goroutine 非常轻量，适合高并发",
    ];
    for content in &msgs {
        let msg = BroadcastMessage {
            content: (*content).to_string(),
            raw_input: (*content).to_string(),
            context: BrainContext {
                current_date: "2026-04-08".into(),
                cwd: "/test".into(),
                git_branch: Some("main".into()),
                platform: "darwin".into(),
            },
            timestamp: Utc::now(),
        };
        memory.store_broadcast(&msg).unwrap();
    }

    let results = memory.recall_for_context("Rust 异步并发", 5);
    println!("[记忆脑测试] 召回 {} 条记忆", results.len());
    for r in &results {
        let preview: String = r.content.chars().take(60).collect();
        println!("  - [{}] {preview} (importance={:.2})", r.id, r.importance);
    }

    let stats = memory.stats().unwrap();
    assert_eq!(stats.l3_count, 3, "L3 应有 3 条原始记忆");
    assert!(stats.l2_count >= 1, "L2 应有短期记忆");
    println!(
        "[记忆脑测试] 统计: L0={}, L1={}, L2={}, L3={}",
        stats.l0_count, stats.l1_count, stats.l2_count, stats.l3_count
    );

    std::mem::forget(tmp);
}
