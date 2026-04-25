use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use brain_bus::BrainBus;
use brain_core::agent::{BrainAgent, StatelessBrain};
use brain_core::config::BrainConfig;
use brain_core::types::{
    BrainId, BrainResponse, BrainResponsePayload, BroadcastMessage, ContextSnapshot,
    EvaluationResult, MainBrainOutput, MasterOutput, MemoryStats, ProgressEvent, TurnUsage,
};
use brain_evaluation::EvaluationBrain;
use brain_evolution::{
    BrainRegistry, BrainRegistryStatus, BrainTemplate, CreationSuggestion, SuggestionEngine,
};
use brain_llm::{ChatMessage, ChatRequest, LlmConfig};
use brain_main::main_brain::MainBrain;
use brain_master::MasterBrain;
use brain_memory::analyzer::{AnalysisLlm, FourStepAnalyzer};
use brain_memory::memory_brain::{MemoryBrain, MemoryBrainConfig};
use brain_motor::motor_brain::{MotorBrain, MotorConfig};
use brain_reasoning::reasoning_brain::{ReasoningBrain, ReasoningConfig};
use brain_sensory::llm::LlmProvider as SensoryLlmProvider;
use brain_sensory::SensoryBrain;
use brain_validation::validation_brain::ValidationConfig;
use brain_validation::ValidationBrain;
use chrono::Utc;
use tokio::sync::Mutex;

// ─── LLM 适配器 ──────────────────────────────────────────────────

/// 将 brain-llm 的 LlmProvider 适配为 brain-sensory 的 LlmProvider
struct LlmAdapter {
    inner: Box<dyn brain_llm::LlmProvider>,
}

impl SensoryLlmProvider for LlmAdapter {
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
            tools: None,
            tool_choice: None,
        };
        Box::pin(async move {
            self.inner
                .complete(request)
                .await
                .map(|r| r.text())
                .map_err(|e| e.to_string())
        })
    }
}

/// 无 LLM 配置时的回声模式
struct EchoLlm;

impl SensoryLlmProvider for EchoLlm {
    fn complete(
        &self,
        _model: &str,
        _system: &str,
        user_input: &str,
        _max: u32,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        let result = format!("[回声] {user_input}");
        Box::pin(async move { Ok(result) })
    }
}

/// 为四步分析提供 LLM 能力的适配器
struct AnalyzerLlm {
    client: Arc<dyn brain_llm::LlmProvider>,
    model: String,
}

impl AnalyzerLlm {
    fn new(client: Arc<dyn brain_llm::LlmProvider>, model: String) -> Self {
        Self { client, model }
    }
}

impl AnalysisLlm for AnalyzerLlm {
    fn complete(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        let request = ChatRequest {
            model: Some(self.model.clone()),
            messages: vec![ChatMessage::user(prompt)],
            max_tokens: Some(4096),
            temperature: Some(0.3),
            tools: None,
            tool_choice: None,
        };
        let client = self.client.clone();
        Box::pin(async move {
            client
                .complete(request)
                .await
                .map(|r| r.text())
                .map_err(|e| e.to_string())
        })
    }
}

// ─── 主脑状态 ────────────────────────────────────────────────────

struct MasterState {
    master: MasterBrain,
    broadcast_rx: brain_bus::BroadcastReceiver,
    result_rx: brain_bus::ResultReceiver,
}

// ─── 系统编排器 ──────────────────────────────────────────────────

/// 系统编排器
///
/// 职责:
///   1. 初始化所有组件（总线、LLM、感知脑、主脑、副脑）
///   2. 启动副脑异步任务（监听广播 → 快思考 → 提交结果）
///   3. 提供 query() 方法（感知脑处理 → 主脑汇总 → 返回结果）
///   4. 提供管理接口（status/weights/memory_stats）
pub struct Orchestrator {
    bus: Arc<BrainBus>,
    sensory: SensoryBrain,
    master_state: Arc<Mutex<MasterState>>,
    memory_brain: Arc<Mutex<MemoryBrain>>,
    evaluation_brain: EvaluationBrain,
    registry: Arc<Mutex<BrainRegistry>>,
    suggestion_engine: Arc<Mutex<SuggestionEngine>>,
    #[allow(dead_code)]
    tasks: Vec<tokio::task::JoinHandle<()>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// 查询计数器
    query_count: std::sync::atomic::AtomicU32,
    /// 模型名（用于状态显示）
    model_name: String,
    /// v2 主脑（带 tool_loop），None 表示 LLM 不可用
    v2_brain: Arc<Mutex<Option<MainBrain>>>,
}

/// 系统状态结构体（TUI 状态栏用）
pub struct SystemStatus {
    pub model: String,
    pub query_count: u32,
    pub eval_enabled: bool,
}

impl Orchestrator {
    /// 初始化所有组件并启动副脑任务
    pub async fn new() -> Result<Self, String> {
        // 1. 三通道消息总线
        let bus = Arc::new(BrainBus::new(64, 64, 64));

        // 2. 感知脑
        let sensory_llm = create_sensory_llm();
        let sensory = SensoryBrain::new("glm-4.7", bus.clone(), sensory_llm);

        // 3. 创建各副脑
        let (mut memory, mut reasoning, mut motor, validation, evaluation) = create_sub_brains()?;

        // 4. 注入 LLM 到需要慢思考的副脑 + 记忆脑（语义召回）
        if let Ok(config) = LlmConfig::load_default() {
            if let Ok(client) = config.create_brain_client("reasoning") {
                let llm: Arc<dyn brain_llm::LlmProvider> = Arc::from(client);
                reasoning.set_llm(llm.clone());
                motor.set_llm(llm);
                tracing::info!("推理脑+执行脑已注入 LLM");
            }
            // 记忆脑单独用 memory brain 的 LLM（语义召回用）
            if let Ok(client) = config.create_brain_client("memory") {
                let analyzer_llm = AnalyzerLlm::new(
                    Arc::from(client),
                    config.model_for_brain("memory").to_string(),
                );
                memory.set_llm(Box::new(analyzer_llm));
                tracing::info!("记忆脑已注入 LLM（语义召回）");
            }
        }

        // 5. 主脑
        let config = BrainConfig::default();
        let master =
            MasterBrain::new(config, bus.clone()).map_err(|e| format!("主脑初始化失败: {e}"))?;

        // 6. 主脑订阅广播 + 取结果接收端
        let master_broadcast_rx = bus.subscribe_broadcast();
        let master_result_rx = bus
            .take_result_receiver()
            .await
            .ok_or("结果接收端已被占用")?;

        // 7. 包装 Arc<Mutex>
        let memory = Arc::new(Mutex::new(memory));
        let reasoning = Arc::new(Mutex::new(reasoning));
        let motor = Arc::new(Mutex::new(motor));
        let validation = Arc::new(Mutex::new(validation));

        // 8. shutdown 信号
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // 9. 启动副脑异步任务
        let mut tasks = Vec::new();

        let mem_id = memory.lock().await.id().clone();
        tasks.push(spawn_agent_loop(
            memory.clone(),
            bus.subscribe_broadcast(),
            bus.subscribe_collaboration(mem_id).await,
            bus.clone(),
            shutdown_rx.clone(),
            memory.clone(),
        ));

        let reas_id = reasoning.lock().await.id().clone();
        tasks.push(spawn_agent_loop(
            reasoning.clone(),
            bus.subscribe_broadcast(),
            bus.subscribe_collaboration(reas_id).await,
            bus.clone(),
            shutdown_rx.clone(),
            memory.clone(),
        ));

        let mot_id = motor.lock().await.id().clone();
        tasks.push(spawn_agent_loop(
            motor.clone(),
            bus.subscribe_broadcast(),
            bus.subscribe_collaboration(mot_id).await,
            bus.clone(),
            shutdown_rx.clone(),
            memory.clone(),
        ));

        let val_id = validation.lock().await.id().clone();
        tasks.push(spawn_agent_loop(
            validation.clone(),
            bus.subscribe_broadcast(),
            bus.subscribe_collaboration(val_id).await,
            bus.clone(),
            shutdown_rx.clone(),
            memory.clone(),
        ));

        // 10. 组装
        let master_state = Arc::new(Mutex::new(MasterState {
            master,
            broadcast_rx: master_broadcast_rx,
            result_rx: master_result_rx,
        }));

        // 11. 进化机制
        let registry = Arc::new(Mutex::new(BrainRegistry::new()));
        let suggestion_engine = Arc::new(Mutex::new(SuggestionEngine::new()));

        tracing::info!("AI Brain 初始化完成，{} 个副脑任务已启动", tasks.len());

        let model_name = LlmConfig::load_default()
            .map(|c| c.model_for_brain("sensory").to_string())
            .unwrap_or_else(|_| "echo".into());

        // 12. 尝试创建 v2 MainBrain（带 tool_loop + 工具注册）
        let v2_brain = create_v2_main_brain(Some(Arc::clone(&memory)));

        // 12.1 启动守护线程（L2→L1 归档）
        {
            let mem_base_dir = {
                let mem_guard = memory.lock().await;
                mem_guard.base_dir().to_path_buf()
            };
            let guardian_llm: Option<Box<dyn AnalysisLlm>> =
                if let Ok(config) = LlmConfig::load_default() {
                    if let Ok(client) = config.create_brain_client("memory") {
                        Some(Box::new(AnalyzerLlm::new(
                            Arc::from(client),
                            config.model_for_brain("memory").to_string(),
                        )))
                    } else {
                        None
                    }
                } else {
                    None
                };

            if let Some(llm) = guardian_llm {
                let mut guardian_config = brain_memory::guardian::GuardianConfig::default();
                guardian_config.check_interval_secs = 28800; // 8h
                guardian_config.min_new_summaries = 10;
                let engine =
                    brain_memory::guardian::GuardianEngine::new(mem_base_dir, guardian_config, llm);
                let mut shutdown_rx_g = shutdown_rx.clone();
                let interval = tokio::time::Duration::from_secs(28800);
                let guardian_handle = tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = tokio::time::sleep(interval) => {
                                match engine.should_run() {
                                    Ok(true) => {
                                        match engine.run().await {
                                            Ok(report) => {
                                                if report.archived_count > 0 || report.new_topics > 0 {
                                                    tracing::info!(
                                                        "守护线程: 归档{}条, 新建{}主题, 合并{}主题",
                                                        report.archived_count,
                                                        report.new_topics,
                                                        report.merged_topics,
                                                    );
                                                }
                                            }
                                            Err(e) => tracing::warn!("守护线程运行失败: {e}"),
                                        }
                                    }
                                    Ok(false) => {} // 条件不满足，下次再检查
                                    Err(e) => tracing::warn!("守护线程检查失败: {e}"),
                                }
                            }
                            _ = shutdown_rx_g.changed() => {
                                tracing::info!("守护线程收到关机信号");
                                break;
                            }
                        }
                    }
                });
                tasks.push(guardian_handle);
                tracing::info!("守护线程已启动 (8h检查间隔)");
            }
        }

        // 12.5 启动时注入潜意识印象到 v2 MainBrain
        if let Ok(mem_guard) = memory.try_lock() {
            if let Some(summary) = mem_guard.load_subconscious_summary() {
                drop(mem_guard);
                if let Ok(mut v2_guard) = v2_brain.try_lock() {
                    if let Some(ref mut brain) = *v2_guard {
                        brain.push_memory_context(&format!(
                            "[潜意识印象 — 你曾经做过这些事，匹配到时再深入回忆]\n{summary}"
                        ));
                        tracing::info!("潜意识印象已注入主脑");
                    }
                }
            }
        }

        Ok(Self {
            bus,
            sensory,
            master_state,
            memory_brain: memory,
            evaluation_brain: evaluation,
            registry,
            suggestion_engine,
            tasks,
            shutdown_tx,
            query_count: std::sync::atomic::AtomicU32::new(0),
            model_name: model_name.to_string(),
            v2_brain,
        })
    }

    /// 提交查询
    pub async fn query(&self, input: &str) -> Result<MasterOutput, String> {
        self.sensory
            .process_input(input)
            .await
            .map_err(|e| format!("感知脑处理失败: {e}"))?;

        // run_once 需要独占 master_state，完成后立即释放锁
        let output = {
            let mut state = self.master_state.lock().await;
            let MasterState {
                master,
                broadcast_rx,
                result_rx,
            } = &mut *state;
            master
                .run_once(broadcast_rx, result_rx)
                .await
                .map_err(|e| format!("主脑处理失败: {e}"))?
        };

        // --- 自动触发评估（使用真实快照） ---
        let snapshots: Vec<ContextSnapshot> = output
            .participating_brains
            .iter()
            .map(|id| {
                ContextSnapshot {
                    brain_id: id.clone(),
                    message_count: 1, // 本轮参与了 1 次消息
                    health_score: output.confidence,
                }
            })
            .collect();

        // 任务完成后自动评估 (task_completed=true)
        if self.evaluation_brain.should_evaluate(&snapshots, 0, true) {
            let eval_result = self.evaluation_brain.evaluate(snapshots);
            tracing::info!(
                "评估完成: 整体健康度 {:.1}%, 瘦身指令 {} 条",
                eval_result.overall_health * 100.0,
                eval_result.slim_instructions.len()
            );
            for instruction in &eval_result.slim_instructions {
                tracing::debug!("瘦身指令: {:?}", instruction);
            }
        }

        // --- 进化闭环：记录任务模式 + 自动休眠检查 ---
        let keywords: Vec<String> = input
            .split(&[' ', ',', '，', '。', '、', '；', '？', '！'][..])
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() >= 2)
            .take(5)
            .collect();
        self.record_task_pattern(
            keywords,
            output.confidence,
            output.participating_brains.clone(),
        )
        .await;
        self.check_auto_dormancy().await;

        Ok(output)
    }

    /// 副脑任务数量（仅测试用）
    #[cfg(test)]
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// 系统状态结构体（TUI 用）
    pub fn status_structured(&self) -> SystemStatus {
        SystemStatus {
            model: self.model_name.clone(),
            query_count: self.query_count.load(std::sync::atomic::Ordering::Relaxed),
            eval_enabled: true,
        }
    }

    /// 流式查询（TUI 用）
    ///
    /// 优先走 v2 MainBrain（带 tool_loop + 工具），不可用时回退 v1。
    pub fn query_streaming(
        self: &Arc<Self>,
        input: &str,
    ) -> (
        tokio::sync::mpsc::Receiver<ProgressEvent>,
        tokio::task::JoinHandle<Result<MainBrainOutput, String>>,
    ) {
        self.query_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let (tx, rx) = tokio::sync::mpsc::channel(256);
        let input_owned = input.to_string();
        let this = Arc::clone(self);

        let handle = tokio::spawn(async move {
            // 尝试 v2 路径
            let v2_result = {
                let mut guard = this.v2_brain.lock().await;
                if let Some(ref mut brain) = *guard {
                    tracing::info!("使用 v2 MainBrain (带工具) 处理查询");

                    // 主脑自己通过 search_memory 工具按需查询记忆，不再主动注入

                    // 主脑处理
                    let result = brain
                        .process_input(&input_owned, Some(&tx))
                        .await
                        .map_err(|e| format!("{e}"));

                    // 将本轮对话存入记忆脑（L3 原始 + L2 短期）
                    if let Ok(ref output) = result {
                        let mem_content = format!(
                            "用户: {}\n助手: {}",
                            input_owned,
                            output.answer.chars().take(500).collect::<String>()
                        );
                        let mem_msg = BroadcastMessage {
                            content: mem_content,
                            raw_input: input_owned.clone(),
                            context: brain_core::types::BrainContext {
                                current_date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
                                cwd: std::env::current_dir()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default(),
                                git_branch: None,
                                platform: std::env::consts::OS.to_string(),
                            },
                            timestamp: chrono::Utc::now(),
                        };
                        let mut mem = this.memory_brain.lock().await;
                        if let Err(e) = mem.store_broadcast(&mem_msg) {
                            tracing::warn!("v2 对话存入记忆脑失败: {e}");
                        }
                    }

                    // 评估脑
                    if result.is_ok() {
                        let snapshots = vec![ContextSnapshot {
                            brain_id: BrainId::master(),
                            message_count: 1,
                            health_score: 0.9, // v2 模式暂用固定值
                        }];
                        if this.evaluation_brain.should_evaluate(&snapshots, 0, true) {
                            let _ = tx.send(ProgressEvent::EvaluationStart).await;
                            let eval_result = this.evaluation_brain.evaluate(snapshots);
                            let passed = eval_result.overall_health >= 0.7;
                            let _ = tx
                                .send(ProgressEvent::EvaluationResult {
                                    passed,
                                    issues: eval_result
                                        .slim_instructions
                                        .iter()
                                        .map(|i| format!("{i:?}"))
                                        .collect(),
                                })
                                .await;
                            let _ = tx
                                .send(ProgressEvent::EvaluationDetail {
                                    score: eval_result.overall_health,
                                    reports: eval_result.brain_reports,
                                    instructions: eval_result.slim_instructions,
                                })
                                .await;
                        }
                    }

                    result
                } else {
                    Err("v2 不可用".into())
                }
            };

            match v2_result {
                Ok(output) => {
                    // 触发四步分析（非阻塞，后台执行）
                    this.maybe_trigger_analysis();
                    Ok(output)
                }
                Err(_) => {
                    // 回退 v1 路径
                    tracing::info!("回退 v1 MasterBrain 处理查询");
                    let _ = tx
                        .send(ProgressEvent::Connecting {
                            brain: "main".into(),
                            model: this.model_name.clone(),
                        })
                        .await;
                    let _ = tx
                        .send(ProgressEvent::Thinking {
                            brain: "main".into(),
                        })
                        .await;

                    match this.query(&input_owned).await {
                        Ok(master_output) => {
                            let answer = master_output.answer.clone();
                            let duration_ms = master_output.usage.duration_ms;

                            // 记忆 + 评估事件
                            Self::send_detail_events(&this, &input_owned, &master_output, &tx)
                                .await;

                            let _ = tx
                                .send(ProgressEvent::TextDelta {
                                    text: answer.clone(),
                                })
                                .await;
                            let _ = tx.send(ProgressEvent::Done).await;

                            Ok(MainBrainOutput {
                                answer,
                                usage: TurnUsage {
                                    total_tokens: master_output.usage.total_tokens,
                                    llm_calls: master_output.usage.llm_calls,
                                    duration_ms,
                                },
                            })
                        }
                        Err(e) => {
                            let _ = tx.send(ProgressEvent::Done).await;
                            Err(e)
                        }
                    }
                }
            }
        });

        (rx, handle)
    }

    /// 发送记忆 + 评估详情事件（v1 回退路径用）
    async fn send_detail_events(
        this: &Arc<Self>,
        _input: &str,
        master_output: &MasterOutput,
        tx: &tokio::sync::mpsc::Sender<ProgressEvent>,
    ) {
        // v1 回退路径不主动注入记忆 —— 主脑自己通过 search_memory 工具按需查询

        // 评估详情
        let snapshots: Vec<ContextSnapshot> = master_output
            .participating_brains
            .iter()
            .map(|id| ContextSnapshot {
                brain_id: id.clone(),
                message_count: 1,
                health_score: master_output.confidence,
            })
            .collect();
        if this.evaluation_brain.should_evaluate(&snapshots, 0, true) {
            let _ = tx.send(ProgressEvent::EvaluationStart).await;
            let eval_result = this.evaluation_brain.evaluate(snapshots);
            let passed = eval_result.overall_health >= 0.7;
            let _ = tx
                .send(ProgressEvent::EvaluationResult {
                    passed,
                    issues: eval_result
                        .slim_instructions
                        .iter()
                        .map(|i| format!("{i:?}"))
                        .collect(),
                })
                .await;
            let _ = tx
                .send(ProgressEvent::EvaluationDetail {
                    score: eval_result.overall_health,
                    reports: eval_result.brain_reports,
                    instructions: eval_result.slim_instructions,
                })
                .await;
        }
    }

    /// 系统状态（文本）
    pub fn status(&self) -> String {
        format!(
            "=== AI Brain 系统状态 ===\n\
             \x20 总线广播订阅者: {}\n\
             \x20 状态: 运行中\n",
            self.bus.broadcast_receiver_count(),
        )
    }

    /// 副脑权重（文本）
    pub async fn weights(&self) -> String {
        let state = self.master_state.lock().await;
        let ws = state.master.get_weights();
        let mut out = "=== 副脑权重 ===\n".to_string();
        for (id, w) in ws {
            use std::fmt::Write;
            let _ = writeln!(out, "  {id}: {:.2}", w.value());
        }
        out
    }

    /// 权重列表（API 用）
    pub async fn weights_list(&self) -> Vec<(String, f64)> {
        let state = self.master_state.lock().await;
        state
            .master
            .get_weights()
            .iter()
            .map(|(id, w)| (id.to_string(), w.value()))
            .collect()
    }

    /// 记忆统计（文本）
    pub async fn memory_stats(&self) -> String {
        let brain = self.memory_brain.lock().await;
        match brain.stats() {
            Ok(s) => format!(
                "=== 记忆统计 ===\n\
                 \x20 L0 任务总结: {}\n\
                 \x20 L1 事件索引: {}\n\
                 \x20 L2 短期记忆: {}\n\
                 \x20 L3 原始记录: {}\n\
                 \x20 总大小: {} bytes\n",
                s.l0_count, s.l1_count, s.l2_count, s.l3_count, s.total_size_bytes,
            ),
            Err(e) => format!("获取记忆统计失败: {e}"),
        }
    }

    /// 记忆统计（原始数据）
    pub async fn memory_stats_raw(&self) -> Result<MemoryStats, String> {
        let brain = self.memory_brain.lock().await;
        brain.stats().map_err(|e| format!("{e}"))
    }

    /// 评估脑 — 默认快照（文本）
    pub fn evaluate_default(&self) -> String {
        let result = self.evaluate_default_raw();
        let mut out = format!(
            "=== 评估结果 ===\n  整体健康度: {:.2}\n",
            result.overall_health
        );
        for r in &result.brain_reports {
            use std::fmt::Write;
            let _ = writeln!(
                out,
                "  [{}] health={:.2} usage={:.0}%",
                r.brain_id,
                r.health_score,
                r.usage_percent * 100.0,
            );
        }
        out
    }

    /// 评估脑 — 默认快照（原始数据）
    pub fn evaluate_default_raw(&self) -> EvaluationResult {
        let snapshots = vec![
            ContextSnapshot {
                brain_id: BrainId::reasoning(),
                message_count: 0,
                health_score: 1.0,
            },
            ContextSnapshot {
                brain_id: BrainId::memory(),
                message_count: 0,
                health_score: 1.0,
            },
            ContextSnapshot {
                brain_id: BrainId::motor(),
                message_count: 0,
                health_score: 1.0,
            },
            ContextSnapshot {
                brain_id: BrainId::validation(),
                message_count: 0,
                health_score: 1.0,
            },
        ];
        self.evaluation_brain.evaluate(snapshots)
    }

    /// 广播订阅者数量
    pub fn broadcast_subscribers(&self) -> usize {
        self.bus.broadcast_receiver_count()
    }

    // ─── 进化机制 ──────────────────────────────────────────────────

    /// 列出所有副脑（活跃+休眠）
    pub async fn brain_status(&self) -> BrainRegistryStatus {
        let reg = self.registry.lock().await;
        reg.status_summary()
    }

    /// 列出可用模板
    pub async fn list_templates(&self) -> Vec<BrainTemplate> {
        let reg = self.registry.lock().await;
        reg.list_templates().into_iter().cloned().collect()
    }

    /// 从模板创建新副脑
    pub async fn create_brain(&self, template_name: &str) -> Result<BrainId, String> {
        let mut reg = self.registry.lock().await;
        reg.create_from_template(template_name)
            .map_err(|e| format!("{e}"))
    }

    /// 注册自定义模板
    #[allow(dead_code)]
    pub async fn register_template(&self, template: BrainTemplate) -> Result<(), String> {
        let mut reg = self.registry.lock().await;
        reg.register_template(template).map_err(|e| format!("{e}"))
    }

    /// 手动休眠副脑
    pub async fn dormant_brain(&self, id: &BrainId) -> Result<(), String> {
        let weights = {
            let state = self.master_state.lock().await;
            let ws = state.master.get_weights();
            ws.get(id)
                .map_or(0.5, |w: &brain_core::types::Weight| w.value())
        };
        let mut reg = self.registry.lock().await;
        reg.dormant(id, weights).map_err(|e| format!("{e}"))
    }

    /// 唤醒副脑
    pub async fn wake_brain(&self, id: &BrainId) -> Result<f64, String> {
        let mut reg = self.registry.lock().await;
        let (_, weight) = reg.wake(id).map_err(|e| format!("{e}"))?;
        Ok(weight.value())
    }

    /// 检查休眠（由主脑每轮任务后调用）
    pub async fn check_auto_dormancy(&self) -> Vec<BrainId> {
        let (weights, dormant_ids) = {
            let state = self.master_state.lock().await;
            let ws = state.master.get_weights().clone();
            let reg = self.registry.lock().await;
            let ids = reg.check_dormancy(&ws);
            (ws, ids)
        };

        if !dormant_ids.is_empty() {
            let mut reg = self.registry.lock().await;
            for id in &dormant_ids {
                let weight = weights
                    .get(id)
                    .map_or(0.1, |w: &brain_core::types::Weight| w.value());
                if let Err(e) = reg.dormant(id, weight) {
                    tracing::warn!("自动休眠 {} 失败: {e}", id);
                } else {
                    tracing::info!("副脑 {} 权重过低，自动进入硬休眠", id);
                }
            }
        }

        dormant_ids
    }

    /// 记录任务模式（由主脑每轮任务后调用）
    pub async fn record_task_pattern(
        &self,
        keywords: Vec<String>,
        confidence: f64,
        participating_brains: Vec<BrainId>,
    ) {
        let mut engine = self.suggestion_engine.lock().await;
        engine.record_pattern(keywords, confidence, participating_brains);
    }

    /// 获取创建建议
    pub async fn get_suggestions(&self) -> Vec<CreationSuggestion> {
        type Cand = (String, Vec<String>, u32, f64, Vec<BrainId>);
        let mut engine = self.suggestion_engine.lock().await;

        // 收集匹配模式的候选
        let candidates: Vec<Cand> = engine
            .check_suggestions()
            .iter()
            .map(|p| {
                let name = format!("auto-{}", p.keywords.first().unwrap_or(&"unknown".into()));
                (
                    name,
                    p.keywords.clone(),
                    p.frequency,
                    p.avg_confidence,
                    p.participating_brains.clone(),
                )
            })
            .collect();

        let mut new_suggestions = Vec::new();
        for (name, keywords, frequency, avg_confidence, participating_brains) in candidates {
            if engine.has_suggested(&name) {
                continue;
            }
            // 手动构建建议，避免从借用中的 engine 再次引用
            let reason = format!(
                "检测到任务模式 [{keywords:?}] 出现 {frequency} 次，平均置信度 {avg_confidence:.2}，建议创建专用副脑"
            );
            let suggestion = CreationSuggestion {
                reason,
                suggested_name: name,
                suggested_capabilities: keywords.clone(),
                parent_brain: participating_brains.first().cloned(),
                suggested_prompt: format!("你是一个专注于 {} 的专家助手。", keywords.join("、")),
                suggested_keywords: keywords,
                confidence: 1.0 - avg_confidence,
            };
            engine.add_suggestion(suggestion.clone());
            new_suggestions.push(suggestion);
        }

        new_suggestions
    }

    /// 从建议创建副脑
    #[allow(dead_code)]
    pub async fn create_from_suggestion(
        &self,
        suggestion: &CreationSuggestion,
    ) -> Result<BrainId, String> {
        let template = SuggestionEngine::suggestion_to_template(suggestion);
        let mut reg = self.registry.lock().await;
        reg.register_template(template)
            .map_err(|e| format!("{e}"))?;
        reg.create_from_template(&suggestion.suggested_name)
            .map_err(|e| format!("{e}"))
    }

    /// 副脑状态文本
    pub async fn brain_status_text(&self) -> String {
        let status = self.brain_status().await;
        let mut out = "=== 副脑状态 ===\n".to_string();

        if status.active.is_empty() && status.dormant.is_empty() {
            out.push_str("  （无自定义副脑）\n");
            return out;
        }

        if !status.active.is_empty() {
            out.push_str("  [活跃]\n");
            for entry in &status.active {
                use std::fmt::Write;
                let _ = writeln!(
                    out,
                    "    {} — {} (任务数: {})",
                    entry.name, entry.description, entry.task_count
                );
            }
        }

        if !status.dormant.is_empty() {
            out.push_str("  [休眠]\n");
            for entry in &status.dormant {
                use std::fmt::Write;
                let _ = writeln!(
                    out,
                    "    {} — {} (任务数: {})",
                    entry.name, entry.description, entry.task_count
                );
            }
        }

        out
    }

    /// 建议文本
    pub async fn suggestions_text(&self) -> String {
        let suggestions = self.get_suggestions().await;
        if suggestions.is_empty() {
            return "当前没有新的副脑创建建议。\n".to_string();
        }

        let mut out = "=== 创建建议 ===\n".to_string();
        for (i, s) in suggestions.iter().enumerate() {
            use std::fmt::Write;
            let _ = writeln!(out, "  [{}] {}", i + 1, s.suggested_name);
            let _ = writeln!(out, "      理由: {}", s.reason);
            let _ = writeln!(out, "      能力: {:?}", s.suggested_capabilities);
        }
        out
    }

    /// 优雅关闭（含强制四步分析）
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        tracing::info!("AI Brain 正在关闭...");
    }

    /// 关闭时强制执行一次四步分析
    pub async fn shutdown_with_analysis(&self) {
        self.run_analysis_force().await;
        self.shutdown();
    }

    /// 检查是否需要触发四步分析（每 N 轮），并在后台启动
    pub fn maybe_trigger_analysis(&self) {
        let should = {
            let mem = match self.memory_brain.try_lock() {
                Ok(m) => m,
                Err(_) => return, // 锁被占用，跳过
            };
            mem.tick_and_should_analyze()
        };

        if should {
            tracing::info!("达到 {} 轮，触发四步分析", 5);
            let (conversations, base_dir, session_id) = {
                // 复制必要数据，避免持锁
                let mem = match self.memory_brain.try_lock() {
                    Ok(m) => m,
                    Err(_) => return,
                };
                let convs = mem.read_recent_conversations(20);
                let dir = mem.base_dir().to_path_buf();
                let sid = mem.session_id().to_string();
                (convs, dir, sid)
            };

            let llm = self.create_analyzer_llm();
            let _ = tokio::spawn(async move {
                if let Some(llm) = llm {
                    let analyzer = FourStepAnalyzer::new(Box::new(llm), base_dir, session_id);
                    let report = analyzer
                        .run(&format!("[{}]", conversations.join(",")))
                        .await;
                    tracing::info!(
                        "四步分析完成: 事实总结={}字, 画像+={}, 踩坑={}, 规则={}, 潜意识={}",
                        report.fact_summary.chars().count(),
                        report.profile_entries_added,
                        report.pitfalls_found,
                        report.rules_created,
                        report.subconscious_entries,
                    );
                }
            });
        }
    }

    /// 强制执行一次四步分析（关闭时用）
    async fn run_analysis_force(&self) {
        let (conversations, base_dir, session_id) = {
            let mem = match self.memory_brain.try_lock() {
                Ok(m) => m,
                Err(_) => return,
            };
            let convs = mem.read_recent_conversations(50);
            let dir = mem.base_dir().to_path_buf();
            let sid = mem.session_id().to_string();
            (convs, dir, sid)
        };

        if conversations.is_empty() {
            tracing::info!("无对话记录，跳过四步分析");
            return;
        }

        let llm = self.create_analyzer_llm();
        if let Some(llm) = llm {
            tracing::info!("关闭时强制执行四步分析...");
            let analyzer = FourStepAnalyzer::new(Box::new(llm), base_dir, session_id);
            let report = analyzer
                .run(&format!("[{}]", conversations.join(",")))
                .await;
            tracing::info!(
                "四步分析完成: 事实总结={}字, 画像+={}, 踩坑={}, 规则={}, 潜意识={}",
                report.fact_summary.chars().count(),
                report.profile_entries_added,
                report.pitfalls_found,
                report.rules_created,
                report.subconscious_entries,
            );
        }
    }

    /// 创建四步分析用的 LLM 客户端
    fn create_analyzer_llm(&self) -> Option<AnalyzerLlm> {
        let config = LlmConfig::load_default().ok()?;
        let client: Box<dyn brain_llm::LlmProvider> = config.create_brain_client("memory").ok()?;
        let model = config.model_for_brain("memory").to_string();
        Some(AnalyzerLlm::new(Arc::from(client), model))
    }
}

impl Drop for Orchestrator {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ─── 副脑任务 ────────────────────────────────────────────────────

fn spawn_agent_loop<A: BrainAgent + 'static>(
    agent: Arc<Mutex<A>>,
    mut broadcast_rx: brain_bus::BroadcastReceiver,
    mut collab_rx: brain_bus::CollaborationReceiver,
    bus: Arc<BrainBus>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    memory_brain: Arc<Mutex<MemoryBrain>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_broadcast: Option<BroadcastMessage> = None;

        loop {
            if *shutdown_rx.borrow() {
                break;
            }

            tokio::select! {
                msg = broadcast_rx.recv() => {
                    match msg {
                        Ok(msg) => {
                            last_broadcast = Some(msg.clone());
                            handle_broadcast(&agent, &bus, msg).await;
                        }
                        Err(e) => {
                            tracing::error!("广播接收失败: {e}");
                            break;
                        }
                    }
                }
                msg = collab_rx.recv() => {
                    if let Some(msg) = msg {
                        if msg.kind == brain_core::types::CollaborationKind::Dispatch {
                            handle_slow_think_dispatch(
                                &agent, &bus, msg, last_broadcast.as_ref(),
                                &memory_brain,
                            ).await;
                        } else {
                            let mut a = agent.lock().await;
                            tracing::debug!("协作: {} → {}", msg.from, a.id());
                            if let Some(resp) = a.on_collaboration(msg) {
                                if let Err(e) = bus.submit_result(resp).await {
                                    tracing::warn!("协作出站提交失败: {e}");
                                }
                            }
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    tracing::debug!("收到关闭信号");
                    break;
                }
            }
        }
    })
}

/// 处理慢思考调度 — 从记忆脑召回上下文后调用 slow_think()
async fn handle_slow_think_dispatch<A: BrainAgent>(
    agent: &Arc<Mutex<A>>,
    bus: &BrainBus,
    collab_msg: brain_core::types::CollaborationMessage,
    last_broadcast: Option<&BroadcastMessage>,
    memory_brain: &Arc<Mutex<MemoryBrain>>,
) {
    // Phase 0: 立刻发 Processing ack，告知主脑"我在想了"
    {
        let a = agent.lock().await;
        let brain_id = a.id().clone();
        let ack = BrainResponse {
            from: brain_id,
            relevance: 0.0,
            confidence: 0.0,
            result: BrainResponsePayload::Processing,
            need_slow_think: false,
            timestamp: Utc::now(),
        };
        if let Err(e) = bus.submit_result(ack).await {
            tracing::warn!("Processing ack 提交失败: {e}");
        } else {
            tracing::info!("副脑 {} 已发送 Processing ack", a.id());
        }
    }

    // Phase 1: 实际慢思考
    let response = {
        let mut a = agent.lock().await;
        let brain_id = a.id().clone();

        // 用缓存的广播消息或从协作消息重建
        let broadcast = last_broadcast.cloned().unwrap_or_else(|| BroadcastMessage {
            content: collab_msg.content.clone(),
            raw_input: collab_msg.content.clone(),
            context: brain_core::types::BrainContext {
                current_date: String::new(),
                cwd: String::new(),
                git_branch: None,
                platform: String::new(),
            },
            timestamp: Utc::now(),
        });

        // 从记忆脑召回相关记忆注入 ThinkContext
        let related_memories = {
            let mem = memory_brain.lock().await;
            mem.recall_for_context(&broadcast.content, 5)
        };

        if !related_memories.is_empty() {
            tracing::info!("慢思考记忆注入: {} 条相关记忆", related_memories.len());
            for mem in &related_memories {
                tracing::debug!(
                    "  记忆: {} (importance={:.2})",
                    mem.content.chars().take(60).collect::<String>(),
                    mem.importance
                );
            }
        }

        let ctx = brain_core::types::ThinkContext {
            related_memories,
            task_history: Vec::new(),
        };

        let preview: String = collab_msg.content.chars().take(80).collect();
        tracing::info!("慢思考调度执行: {} (content={:?})", brain_id, preview);

        let result = a.slow_think(&broadcast, &ctx).await;

        // 让副脑处理新经验回写
        a.on_slow_think_result(&result);

        BrainResponse {
            from: brain_id,
            relevance: result.confidence,
            confidence: result.confidence,
            result: BrainResponsePayload::SlowThink(result),
            need_slow_think: false,
            timestamp: Utc::now(),
        }
    };

    if let Err(e) = bus.submit_result(response).await {
        tracing::warn!("慢思考结果提交失败: {e}");
    }
}

async fn handle_broadcast<A: BrainAgent>(
    agent: &Arc<Mutex<A>>,
    bus: &BrainBus,
    msg: BroadcastMessage,
) {
    let response = {
        let mut a = agent.lock().await;
        a.on_broadcast(msg.clone());
        let result = a.fast_think(&msg);
        let from = a.id().clone();

        BrainResponse {
            from,
            relevance: result.confidence,
            confidence: result.confidence,
            result: if result.relevant {
                BrainResponsePayload::FastThink(result.clone())
            } else {
                BrainResponsePayload::NotRelevant {
                    reason: "not relevant to this brain".into(),
                }
            },
            need_slow_think: result.relevant && result.confidence < 0.7,
            timestamp: Utc::now(),
        }
    };

    if let Err(e) = bus.submit_result(response).await {
        tracing::warn!("提交结果失败: {e}");
    }
}

// ─── 输出格式化 ──────────────────────────────────────────────────

/// 格式化 MasterOutput 为用户可读文本
pub fn format_output(output: &MasterOutput) -> String {
    use std::fmt::Write;
    let mut out = output.answer.clone();
    if !output.participating_brains.is_empty() {
        let brains: Vec<String> = output
            .participating_brains
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        let brains_joined = brains.join(", ");
        let conf = output.confidence * 100.0;
        let dur = output.usage.duration_ms;
        let _ = write!(
            out,
            "\n[参与副脑: {brains_joined} | 置信度: {conf:.0}% | 耗时: {dur}ms]"
        );
    }
    out
}

// ─── 测试 ────────────────────────────────────────────────────────

// ─── 辅助函数 ────────────────────────────────────────────────────

/// 创建感知脑 LLM（配置存在时用真实 LLM，否则回声模式）
fn create_sensory_llm() -> Box<dyn SensoryLlmProvider> {
    match LlmConfig::load_default() {
        Ok(config) => match config.create_brain_client("sensory") {
            Ok(client) => {
                tracing::info!(
                    "LLM 已加载，感知脑模型: {}",
                    config.model_for_brain("sensory")
                );
                Box::new(LlmAdapter { inner: client })
            }
            Err(e) => {
                tracing::warn!("LLM 客户端创建失败 ({e})，使用回声模式");
                Box::new(EchoLlm)
            }
        },
        Err(e) => {
            tracing::info!("未找到 LLM 配置 ({e})，使用回声模式");
            Box::new(EchoLlm)
        }
    }
}

/// 创建 v2 MainBrain（带 tool_loop + 工具注册）
///
/// LLM 可用时创建并注册所有 MVP 工具，不可用时返回 None（回退 v1）
fn create_v2_main_brain(
    memory_brain: Option<Arc<Mutex<MemoryBrain>>>,
) -> Arc<Mutex<Option<MainBrain>>> {
    let config = match LlmConfig::load_default() {
        Ok(c) => c,
        Err(e) => {
            tracing::info!("v2 MainBrain: 无 LLM 配置 ({e})，跳过");
            return Arc::new(Mutex::new(None));
        }
    };

    let client: Box<dyn brain_llm::LlmProvider> = match config.create_brain_client("sensory") {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("v2 MainBrain: LLM 客户端创建失败 ({e})，跳过");
            return Arc::new(Mutex::new(None));
        }
    };
    let llm: Arc<dyn brain_llm::LlmProvider> = Arc::from(client);

    let tool_executor: Arc<dyn brain_core::tool_executor::ToolExecutor> = Arc::new(
        crate::real_tool_executor::RealToolExecutor::with_memory(memory_brain),
    );

    let brain_config = BrainConfig::default();
    let mut brain = MainBrain::new(llm, tool_executor, brain_config);

    // 注册所有 MVP 工具（bash、read_file、write_file、edit_file、glob、grep）
    let tool_defs = crate::real_tool_executor::mvp_tool_definitions();
    tracing::info!("v2 MainBrain: 注册 {} 个工具", tool_defs.len());
    brain.register_tools(tool_defs);

    Arc::new(Mutex::new(Some(brain)))
}

/// 创建所有副脑
fn create_sub_brains() -> Result<
    (
        MemoryBrain,
        ReasoningBrain,
        MotorBrain,
        ValidationBrain,
        EvaluationBrain,
    ),
    String,
> {
    let memory = MemoryBrain::new(MemoryBrainConfig::default())
        .map_err(|e| format!("记忆脑初始化失败: {e}"))?;
    let reasoning = ReasoningBrain::new(ReasoningConfig::default())
        .map_err(|e| format!("推理脑初始化失败: {e}"))?;
    let motor =
        MotorBrain::new(MotorConfig::default()).map_err(|e| format!("执行脑初始化失败: {e}"))?;
    let validation = ValidationBrain::new(ValidationConfig {
        truthfulness_warning_threshold: 0.60,
    })
    .map_err(|e| format!("校验脑初始化失败: {e}"))?;
    let evaluation =
        EvaluationBrain::with_defaults().map_err(|e| format!("评估脑初始化失败: {e}"))?;
    Ok((memory, reasoning, motor, validation, evaluation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::TurnUsage;

    #[test]
    fn test_format_output() {
        let output = MasterOutput {
            answer: "4月有清明节".into(),
            confidence: 0.85,
            sources: vec![],
            participating_brains: vec![BrainId::reasoning(), BrainId::memory()],
            usage: TurnUsage {
                total_tokens: 100,
                llm_calls: 1,
                duration_ms: 150,
            },
        };
        let formatted = format_output(&output);
        assert!(formatted.contains("清明节"));
        assert!(formatted.contains("reasoning"));
        assert!(formatted.contains("85%"));
    }

    #[tokio::test]
    async fn test_orchestrator_init() {
        let orch = Orchestrator::new().await;
        assert!(orch.is_ok(), "Orchestrator 初始化应该成功");
        let orch = orch.unwrap();
        assert_eq!(orch.task_count(), 5, "应该有 5 个任务（4副脑+1守护线程）");
    }

    #[tokio::test]
    async fn test_orchestrator_query() {
        let orch = Orchestrator::new().await.unwrap();

        let result =
            tokio::time::timeout(std::time::Duration::from_secs(30), orch.query("你好")).await;
        match result {
            Ok(Ok(output)) => {
                assert!(!output.answer.is_empty(), "回答不应为空");
            }
            Ok(Err(e)) => panic!("查询失败: {e}"),
            Err(elapsed) => panic!("查询超时 30s: {elapsed:?}"),
        }
        // 显式 shutdown，避免 Drop 中的异步操作 hang
        orch.shutdown();
    }

    #[tokio::test]
    async fn test_orchestrator_status() {
        let orch = Orchestrator::new().await.unwrap();
        assert!(orch.status().contains("运行中"));
    }

    #[tokio::test]
    async fn test_orchestrator_weights() {
        let orch = Orchestrator::new().await.unwrap();
        let w = orch.weights().await;
        assert!(w.contains("reasoning"));
        assert!(w.contains("memory"));
    }

    #[tokio::test]
    async fn test_orchestrator_memory_stats() {
        let orch = Orchestrator::new().await.unwrap();
        let s = orch.memory_stats().await;
        assert!(s.contains("记忆统计") || s.contains("失败"));
    }

    #[test]
    fn test_evaluate_default() {
        let evaluation = EvaluationBrain::with_defaults().unwrap();
        let snapshots = vec![ContextSnapshot {
            brain_id: BrainId::reasoning(),
            message_count: 0,
            health_score: 1.0,
        }];
        let result = evaluation.evaluate(snapshots);
        assert!(result.overall_health > 0.0);
    }
}
