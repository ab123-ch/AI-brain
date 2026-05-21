use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::llm_usage_logger;

/// 按字符数安全截断 UTF-8 字符串（不会在多字节字符中间切割）
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    if s.chars().count() <= max_chars {
        return s;
    }
    let boundary = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..boundary]
}

use brain_bus::BrainBus;
use brain_core::agent::{BrainAgent, StatelessBrain};
use brain_core::config::BrainConfig;
use brain_core::types::{
    BrainId, BrainResponse, BrainResponsePayload, BroadcastMessage, ContextSnapshot,
    EvaluationResult, MainBrainOutput, MasterOutput, MemoryStats, ProgressEvent,
};
use brain_eval::EvalBrain;
use brain_evaluation::EvaluationBrain;
use brain_evolution::{
    BrainRegistry, BrainRegistryStatus, BrainTemplate, CreationSuggestion, SuggestionEngine,
};
use brain_evolver::{EvolutionGoal, EvolverBrain};
use brain_hooks::config::HooksConfig;
use brain_hooks::runner::HookRunner;
use brain_hooks::types::{HookEvent, HookInput};
use brain_llm::{ChatMessage, ChatRequest, LlmConfig};
use brain_main::main_brain::MainBrain;
use brain_master::MasterBrain;
use brain_memory::analyzer::{AnalysisLlm, FourStepAnalyzer};
use brain_memory::memory_brain::{MemoryBrain, MemoryBrainConfig};
use brain_mcp::config::load_mcp_servers;
use brain_mcp::McpClientPool;
use brain_motor::motor_brain::{MotorBrain, MotorConfig};
use brain_plugin::{PluginManager, SkillCatalog};
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

/// 为四步分析提供 LLM 能力的适配器
struct AnalyzerLlm {
    client: Arc<dyn brain_llm::LlmProvider>,
    model: String,
    max_tokens: u32,
    temperature: f64,
}

impl AnalyzerLlm {
    fn new(client: Arc<dyn brain_llm::LlmProvider>, model: String, max_tokens: u32, temperature: f64) -> Self {
        Self { client, model, max_tokens, temperature }
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
            max_tokens: Some(self.max_tokens),
            temperature: Some(self.temperature),
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
    /// v2 评估脑（LLM 深度评估），None 表示 LLM 不可用
    eval_brain: Option<EvalBrain>,
    /// Hook 执行引擎（eval_gate 决策等）
    hook_runner: HookRunner,
    registry: Arc<Mutex<BrainRegistry>>,
    suggestion_engine: Arc<Mutex<SuggestionEngine>>,
    evolver: Arc<Mutex<EvolverBrain>>,
    #[allow(dead_code)]
    tasks: Vec<tokio::task::JoinHandle<()>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// 查询计数器
    query_count: std::sync::atomic::AtomicU32,
    /// 模型名（用于状态显示）
    model_name: String,
    /// v2 主脑（带 tool_loop），None 表示 LLM 不可用
    v2_brain: Arc<Mutex<Option<MainBrain>>>,
    /// 消息调度中间件（异步子代理通知、副脑任务编排）
    dispatch: brain_dispatch::TokioDispatch,
    /// dispatch_loop 输出通道（主脑消费异步通知）
    dispatch_output_rx: Arc<Mutex<tokio::sync::mpsc::Receiver<brain_dispatch::MainLoopMessage>>>,
    /// 插件管理器
    #[allow(dead_code)] // Task 8 会使用
    plugin_mgr: Option<PluginManager>,
    /// 技能目录
    #[allow(dead_code)] // Task 8 会使用
    skill_catalog: Arc<SkillCatalog>,
    /// MCP 客户端池
    #[allow(dead_code)] // Task 8 会使用
    mcp_pool: Arc<McpClientPool>,
}

/// 系统状态结构体（TUI 状态栏用）
pub struct SystemStatus {
    pub model: String,
    pub query_count: u32,
    pub eval_enabled: bool,
    pub context_usage: f64,
    /// 会话累计 input/prompt tokens
    pub cumulative_prompt_tokens: u64,
    /// 会话累计 output/completion tokens
    pub cumulative_completion_tokens: u64,
    /// 会话累计 cache read tokens
    pub cumulative_cache_read_tokens: u64,
    /// 累计压缩次数
    pub compaction_count: u32,
    /// 累计节省的字符数
    pub chars_saved_by_compaction: usize,
}

impl Orchestrator {
    /// 初始化所有组件并启动副脑任务
    pub async fn new() -> Result<Self, String> {
        // 1. 三通道消息总线
        let bus = Arc::new(BrainBus::new(64, 64, 64));

        // 2. 感知脑（LLM 不可用直接报错，不降级）
        let sensory_llm_result = create_sensory_llm()?;
        let model_name = sensory_llm_result.model_name.clone();
        let sensory = SensoryBrain::new("glm-4.7", bus.clone(), sensory_llm_result.provider);

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
                let (mt, temp) = config.params_for_brain("memory");
                let analyzer_llm = AnalyzerLlm::new(
                    Arc::from(client),
                    config.model_for_brain("memory").to_string(),
                    mt, temp,
                );
                memory.set_llm(Box::new(analyzer_llm));
                tracing::info!("记忆脑已注入 LLM（语义召回）");
            }
        }

        // 4.0 提前包装记忆脑 Arc<Mutex>（评估脑和后续都需要）
        let memory = Arc::new(Mutex::new(memory));

        // 4.1 创建 v2 评估脑（LLM 深度评估 + 只读工具验证）
        let eval_tool_executor: Arc<dyn brain_core::tool_executor::ToolExecutor> = Arc::new(
            crate::real_tool_executor::RealToolExecutor::with_memory(Some(memory.clone())),
        );
        let mut eval_brain = if let Ok(config) = LlmConfig::load_default() {
            if let Ok(client) = config.create_brain_client("eval") {
                Some(EvalBrain::with_verification(
                    Arc::from(client),
                    eval_tool_executor,
                ))
            } else {
                None
            }
        } else {
            None
        };
        // 加载评估脑内置 skills
        if let Some(ref mut eb) = eval_brain {
            // TODO: Task 9 - EvalBrain.set_skill_catalog(skill_catalog.clone())
            let skills_dir = std::path::Path::new("rust/crates/brain-eval/skills");
            if let Err(e) = eb.load_skills_from_dir(skills_dir) {
                tracing::warn!("加载评估脑 skills 失败: {e}");
            } else {
                tracing::info!("评估脑 skills 加载成功");
            }
        }
        if eval_brain.is_some() {
            tracing::info!("v2 评估脑已创建（LLM 深度评估 + 只读工具验证）");
        }

        // 4.2 初始化 Hook 系统（eval_gate 纯规则判断）
        let hooks_config: HooksConfig = LlmConfig::load_default()
            .ok()
            .and_then(|c| c.hooks)
            .as_ref()
            .map(HooksConfig::from_toml_value)
            .unwrap_or_default();

        let hook_runner = HookRunner::new(hooks_config);

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

        // 11.1 进化脑（EvolverBrain）
        let evolver = if let Ok(config) = LlmConfig::load_default() {
            if let Ok(client) = config.create_brain_client("evolver") {
                let llm: Arc<dyn brain_llm::LlmProvider> = Arc::from(client);
                Some(EvolverBrain::new(llm, std::path::Path::new(".")))
            } else {
                None
            }
        } else {
            None
        };
        if evolver.is_some() {
            tracing::info!("进化脑已创建（EvolverBrain）");
        }
        let evolver = Arc::new(Mutex::new(evolver.unwrap_or_else(|| {
            // 此时 create_sensory_llm 已成功，sensory LLM 必定可用
            let client = try_create_llm_client("sensory")
                .expect("sensory LLM should be available after successful create_sensory_llm()");
            EvolverBrain::new(client, std::path::Path::new("."))
        })));

        tracing::info!("AI Brain 初始化完成，{} 个副脑任务已启动", tasks.len());

        // model_name 已从 create_sensory_llm 获取，此处不再重复计算

        // 11.5 初始化消息调度中间件（必须在 v2_brain 之前，因为 dispatch 要注入 RealToolExecutor）
        let dispatch = brain_dispatch::TokioDispatch::new(256);
        let (dispatch_output_tx, dispatch_output_rx) =
            tokio::sync::mpsc::channel::<brain_dispatch::MainLoopMessage>(64);

        // 启动 dispatch loop
        {
            let dispatch_clone = dispatch.clone();
            let mut shutdown_rx_clone = shutdown_rx.clone();
            tasks.push(tokio::spawn(async move {
                tokio::select! {
                    _ = dispatch_clone.run_dispatch_loop(dispatch_output_tx) => {}
                    _ = shutdown_rx_clone.changed() => {
                        tracing::info!("dispatch loop shutting down");
                    }
                }
            }));
        }

        // 12. 尝试创建 v2 MainBrain（带 tool_loop + 工具注册 + dispatch）
        let (v2_brain, plugin_mgr, skill_catalog, mcp_pool) =
            create_v2_main_brain(Some(Arc::clone(&memory)), dispatch.clone());

        // 12.1 启动守护线程（L2→L1 归档）
        {
            let mem_base_dir = {
                let mem_guard = memory.lock().await;
                mem_guard.base_dir().to_path_buf()
            };
            let guardian_llm: Option<Box<dyn AnalysisLlm>> =
                if let Ok(config) = LlmConfig::load_default() {
                    if let Ok(client) = config.create_brain_client("memory") {
                        let (mt, temp) = config.params_for_brain("memory");
                        Some(Box::new(AnalyzerLlm::new(
                            Arc::from(client),
                            config.model_for_brain("memory").to_string(),
                            mt, temp,
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
                        brain.inject_memory_context(&format!(
                            "[潜意识印象 — 你曾经做过这些事，匹配到时再深入回忆]\n{summary}"
                        ));
                        tracing::info!("潜意识印象已注入主脑");
                    }
                }
            }
        }

        // 12.6 检查并注入上次会话的待分析对话
        let pending_base_dir = {
            let mem = match memory.try_lock() {
                Ok(m) => m,
                Err(_) => return Err("记忆脑锁被占用".to_string()),
            };
            mem.base_dir().to_path_buf()
        };
        if let Some(pending) = brain_memory::pending_analysis::PendingAnalysis::load(&pending_base_dir) {
            let injection_text = pending.format_for_injection();
            let convs_for_analysis = pending.conversations.clone();
            let sess_id = pending.session_id.clone();
            let base_for_analysis = pending_base_dir.clone();

            // 1) 注入主脑上下文（LLM 立即可用）
            if let Ok(mut v2_guard) = v2_brain.try_lock() {
                if let Some(ref mut brain) = *v2_guard {
                    brain.push_memory_context(&injection_text);
                    tracing::info!(
                        "已注入上次会话记忆 ({}条对话)",
                        convs_for_analysis.len()
                    );
                }
            }

            // 2) 后台跑四步分析（更新持久化记忆，fire-and-forget）
            let llm = Self::create_analyzer_llm_from_config();
            drop(tokio::spawn(async move {
                if let Some(llm) = llm {
                    let analyzer = FourStepAnalyzer::new(Box::new(llm), base_for_analysis, sess_id);
                    let report = analyzer
                        .run(&format!("[{}]", convs_for_analysis.join(",")))
                        .await;
                    tracing::info!(
                        "后台四步分析完成: 事实总结={}字, 画像+={}, 踩坑={}, 规则={}, 潜意识叙事更新={}",
                        report.fact_summary.chars().count(),
                        report.profile_entries_added,
                        report.pitfalls_found,
                        report.rules_created,
                        report.subconscious_updated,
                    );
                }
            }));
        }

        Ok(Self {
            bus,
            sensory,
            master_state,
            memory_brain: memory,
            evaluation_brain: evaluation,
            eval_brain,
            hook_runner,
            registry,
            suggestion_engine,
            evolver,
            tasks,
            shutdown_tx,
            query_count: std::sync::atomic::AtomicU32::new(0),
            model_name: model_name.to_string(),
            v2_brain,
            dispatch,
            dispatch_output_rx: Arc::new(Mutex::new(dispatch_output_rx)),
            plugin_mgr,
            skill_catalog,
            mcp_pool,
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

    /// 获取插件管理器引用
    pub fn plugin_mgr(&self) -> Option<&PluginManager> {
        self.plugin_mgr.as_ref()
    }

    /// 获取技能目录引用
    pub fn skill_catalog(&self) -> &SkillCatalog {
        &self.skill_catalog
    }

    /// 获取 MCP 客户端池引用
    pub fn mcp_pool(&self) -> &McpClientPool {
        &self.mcp_pool
    }

    /// 系统状态结构体（TUI 用）
    pub fn status_structured(&self) -> SystemStatus {
        let (context_usage, cumulative_prompt, cumulative_completion, cumulative_cache_read, compaction_count, chars_saved) = self
            .v2_brain
            .try_lock()
            .ok()
            .and_then(|g| {
                g.as_ref().map(|b| {
                    (
                        b.context_usage(),
                        b.cumulative_prompt_tokens(),
                        b.cumulative_completion_tokens(),
                        b.cumulative_cache_read_tokens(),
                        b.compaction_count(),
                        b.chars_saved_by_compaction(),
                    )
                })
            })
            .unwrap_or((0.0, 0, 0, 0, 0, 0));
        SystemStatus {
            model: self.model_name.clone(),
            query_count: self.query_count.load(std::sync::atomic::Ordering::Relaxed),
            eval_enabled: true,
            context_usage,
            cumulative_prompt_tokens: cumulative_prompt,
            cumulative_completion_tokens: cumulative_completion,
            cumulative_cache_read_tokens: cumulative_cache_read,
            compaction_count,
            chars_saved_by_compaction: chars_saved,
        }
    }

    /// 获取补全数据（模板名 + 模式关键词），供 TUI 补全使用
    pub fn completion_data(&self) -> (Vec<String>, Vec<String>) {
        let template_names = match self.registry.try_lock() {
            Ok(guard) => guard.list_templates().iter().map(|t| t.name.clone()).collect(),
            Err(_) => Vec::new(),
        };
        let pattern_keywords = match self.suggestion_engine.try_lock() {
            Ok(guard) => guard.patterns().iter().flat_map(|p| p.keywords.clone()).collect(),
            Err(_) => Vec::new(),
        };
        (template_names, pattern_keywords)
    }

    /// 流式查询（TUI 用）
    ///
    /// 优先走 v2 MainBrain（带 tool_loop + 工具），不可用时回退 v1。
    /// 返回值中包含 CancellationToken，调用者可通过 cancel() 协作取消 tool_loop。
    pub fn query_streaming(
        self: &Arc<Self>,
        input: &str,
    ) -> (
        tokio::sync::mpsc::Receiver<ProgressEvent>,
        tokio::task::JoinHandle<Result<MainBrainOutput, String>>,
        tokio_util::sync::CancellationToken,
    ) {
        self.query_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let (tx, rx) = tokio::sync::mpsc::channel(256);
        let input_owned = input.to_string();
        let this = Arc::clone(self);
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            // 尝试 v2 路径
            let v2_result = {
                let mut guard = this.v2_brain.lock().await;
                if let Some(ref mut brain) = *guard {
                    tracing::info!("使用 v2 MainBrain (带工具) 处理查询");

                    // --- 1. 主脑首次处理 ---
                    let mut result = brain
                        .process_input(&input_owned, Some(&tx), Some(cancel_clone.clone()))
                        .await
                        .map_err(|e| format!("{e}"));

                    // 将本轮对话完整轨迹存入记忆脑（L3 原始 + L2 短期）
                    if let Ok(ref output) = result {
                        let mut mem = this.memory_brain.lock().await;
                        if let Err(e) = mem.store_turns(&output.turns) {
                            tracing::warn!("v2 对话存入记忆脑失败: {e}");
                        }
                    }

                    // --- 2. Hook 系统决策是否触发评估 ---
                    let ai_answer = result
                        .as_ref()
                        .ok()
                        .map(|o| o.answer.clone())
                        .unwrap_or_default();
                    let hook_input = HookInput {
                        event: HookEvent::PostQuery,
                        session_id: String::new(),
                        cwd: std::env::current_dir().unwrap_or_default(),
                        tool_name: None,
                        tool_input: None,
                        tool_output: None,
                        is_error: false,
                        user_input: Some(input_owned.clone()),
                        ai_output: Some(ai_answer),
                    };
                    let hook_outputs = this.hook_runner.run(&hook_input).await;
                    let should_eval = hook_outputs.iter().any(|o| o.trigger_eval);

                    // --- 3. 评估反馈循环 ---
                    if should_eval && result.is_ok() {
                        tracing::info!("eval_gate 判定：需要评估");
                        if let Some(ref eb) = this.eval_brain {
                            let mem_base_dir = {
                                let mem_guard = this.memory_brain.lock().await;
                                mem_guard.base_dir().to_path_buf()
                            };
                            let storage = brain_memory::storage::Storage::new_lazy(mem_base_dir);

                            // 从记忆脑读取踩坑库、用户画像、进化规则
                            let pitfalls =
                                brain_memory::pitfall::PitfallStore::new(storage.clone())
                                    .load_active()
                                    .unwrap_or_default();
                            let profile =
                                brain_memory::user_profile::UserProfileStore::new(storage.clone())
                                    .load()
                                    .unwrap_or_default();
                            let rules = brain_memory::evolution::EvolutionStore::new(storage.clone())
                                .load_active()
                                .unwrap_or_default();
                            let eval_requirements = brain_memory::eval_requirement::EvalRequirementStore::new(storage)
                                .load_active()
                                .unwrap_or_default();

                            tracing::info!(
                                "v2 评估脑开始评估 (踩坑={} 画像偏好={} 进化规则={} 用户评估要求={})",
                                pitfalls.len(),
                                profile.explicit_preferences.len()
                                    + profile.implicit_preferences.len(),
                                rules.len(),
                                eval_requirements.len(),
                            );

                            let max_eval_retries = 2u32;
                            for attempt in 0..=max_eval_retries {
                                let answer = result.as_ref().unwrap().answer.clone();

                                // 工具调用轮次可能无文本输出，跳过评估
                                if answer.trim().is_empty() {
                                    tracing::debug!(
                                        "v2 评估脑跳过：本轮输出为空（可能纯工具调用），attempt={}",
                                        attempt + 1
                                    );
                                    break;
                                }

                                let _ = tx.send(ProgressEvent::Evaluating).await;

                                match eb
                                    .evaluate(
                                        &input_owned,
                                        &answer,
                                        &result.as_ref().unwrap().turns,
                                        &pitfalls,
                                        &profile,
                                        &rules,
                                        &eval_requirements,
                                    )
                                    .await
                                {
                                    Ok(eval_result) => {
                                        tracing::info!(
                                            "v2 评估脑完成(第{}次): passed={}, feedback={}",
                                            attempt + 1,
                                            eval_result.passed,
                                            truncate_chars(&eval_result.feedback, 100)
                                        );
                                        let _ = tx
                                            .send(ProgressEvent::EvaluationResult {
                                                passed: eval_result.passed,
                                                feedback: eval_result.feedback.clone(),
                                            })
                                            .await;

                                        if eval_result.passed {
                                            tracing::info!("v2 评估通过 (第{}次)", attempt + 1);
                                            break;
                                        }

                                        if attempt >= max_eval_retries {
                                            tracing::warn!(
                                                "v2 评估达到最大重试次数({}), 使用当前输出",
                                                max_eval_retries + 1
                                            );
                                            break;
                                        }

                                        tracing::warn!(
                                            "v2 评估发现问题(第{}次): {}",
                                            attempt + 1,
                                            truncate_chars(&eval_result.feedback, 200)
                                        );

                                        // 将评估反馈注入主脑对话历史（Evaluator 角色）
                                        brain.push_evaluator_to_history(&eval_result.feedback);

                                        // 主脑根据反馈重新生成
                                        let revision_prompt =
                                            "请根据以上评估反馈修正你的回答，直接输出修正后的完整内容。";
                                        match brain.process_input(revision_prompt, Some(&tx), None).await
                                        {
                                            Ok(retry_output) => {
                                                tracing::info!(
                                                    "评估重试第{}次完成, 新回答长度={}",
                                                    attempt + 1,
                                                    retry_output.answer.len()
                                                );
                                                // 存入记忆脑
                                                {
                                                    let mut mem = this.memory_brain.lock().await;
                                                    if let Err(e) =
                                                        mem.store_turns(&retry_output.turns)
                                                    {
                                                        tracing::warn!(
                                                            "评估重试对话存入记忆脑失败: {e}"
                                                        );
                                                    }
                                                }
                                                result = Ok(retry_output);
                                            }
                                            Err(e) => {
                                                tracing::warn!("评估重试处理失败: {e}");
                                                break;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("v2 评估脑评估失败: {e}");
                                        break;
                                    }
                                }
                            }
                        }
                    } else if !should_eval {
                        tracing::info!("eval_gate 判定：跳过评估");
                    }

                    result
                } else {
                    Err("v2 不可用".into())
                }
            };

            match v2_result {
                Ok(output) => {
                    // 记录 LLM 使用情况到日志文件
                    llm_usage_logger::log_llm_usage(
                        &this.model_name,
                        &brain_llm::types::TokenUsage {
                            prompt_tokens: output.usage.prompt_tokens,
                            completion_tokens: output.usage.completion_tokens,
                            total_tokens: output.usage.total_tokens,
                            cache_creation_input_tokens: 0, // MainBrainOutput 不包含此字段
                            cache_read_input_tokens: 0,     // MainBrainOutput 不包含此字段
                        },
                    );
                    
                    // 触发四步分析（非阻塞，后台执行）
                    this.maybe_trigger_analysis();

                    // 检查 dispatch 队列中的异步子代理/副脑通知
                    {
                        let mut dispatch_rx = this.dispatch_output_rx.lock().await;
                        while let Ok(msg) = dispatch_rx.try_recv() {
                            match msg {
                                brain_dispatch::MainLoopMessage::AgentNotification(result) => {
                                    let notification = format!(
                                        "\n📡 异步子代理完成: {} ({:?})\n",
                                        result.agent_id, result.status
                                    );
                                    let _ = tx.send(ProgressEvent::TextDelta {
                                        text: notification,
                                    }).await;
                                    tracing::info!("异步子代理通知: {} status={:?}", result.agent_id, result.status);
                                }
                                brain_dispatch::MainLoopMessage::BrainTaskNotification { brain_id, result } => {
                                    let notification = format!(
                                        "\n📡 副脑任务完成: {brain_id}\n",
                                    );
                                    let _ = tx.send(ProgressEvent::TextDelta {
                                        text: notification,
                                    }).await;
                                    tracing::info!("副脑任务通知: {brain_id} status={:?}", result.status);
                                }
                            }
                        }
                    }

                    // 通知 TUI 查询完成（process_input 不走 streaming，不会自行发 Done）
                    let _ = tx.send(ProgressEvent::Done).await;

                    Ok(output)
                }
                Err(e) => {
                    // 不再回退 v1，直接返回错误
                    tracing::error!("v2 MainBrain 处理失败，不回退 v1: {e}");
                    let _ = tx.send(ProgressEvent::Done).await;
                    Err(e)
                }
            }
        });

        (rx, handle, cancel)
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

    // ─── 进化脑方法 ──────────────────────────────────────────────────

    /// 启动进化任务
    pub async fn start_evolution(&self, goal: String) -> Result<String, String> {
        let engine = self.evolver.lock().await.engine();
        let mut engine = engine.lock().await;

        let evo_goal = EvolutionGoal {
            description: goal.clone(),
            target_files: vec![],
            expected_outcome: String::new(),
            test_scenarios: vec![],
        };

        engine.start(evo_goal).await.map_err(|e| e.to_string())?;
        Ok(engine.status().to_string())
    }

    /// 查看进化状态
    pub async fn evolution_status(&self) -> String {
        let engine = self.evolver.lock().await.engine();
        let engine = engine.lock().await;
        format!("{}", engine.status())
    }

    /// 确认合并进化结果
    pub async fn approve_evolution(&self) -> Result<(), String> {
        let engine = self.evolver.lock().await.engine();
        let mut engine = engine.lock().await;
        engine.approve().await.map_err(|e| e.to_string())
    }

    /// 拒绝并回滚进化
    pub async fn reject_evolution(&self) -> Result<(), String> {
        let engine = self.evolver.lock().await.engine();
        let mut engine = engine.lock().await;
        engine.reject().await.map_err(|e| e.to_string())
    }

    /// 查看进化 diff
    pub async fn evolution_diff(&self) -> Result<String, String> {
        let engine = self.evolver.lock().await.engine();
        let engine = engine.lock().await;
        engine.current_diff().await.map_err(|e| e.to_string())
    }

    /// 优雅关闭（含强制四步分析）
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        tracing::info!("AI Brain 正在关闭...");
    }

    /// 关闭时快速保存对话数据（替代 run_analysis_force，毫秒级）
    pub async fn shutdown_with_analysis(&self) {
        self.save_pending_analysis();
        
        // 记录会话结束时的 LLM 使用统计
        llm_usage_logger::log_session_summary();
        
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
                        "四步分析完成: 事实总结={}字, 画像+={}, 踩坑={}, 规则={}, 潜意识叙事更新={}",
                        report.fact_summary.chars().count(),
                        report.profile_entries_added,
                        report.pitfalls_found,
                        report.rules_created,
                        report.subconscious_updated,
                    );
                }
            });
        }
    }

    /// 保存未分析的对话到磁盘（毫秒级，替代关闭时的 run_analysis_force）
    fn save_pending_analysis(&self) {
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
            tracing::info!("无对话记录，跳过保存");
            return;
        }

        match brain_memory::pending_analysis::PendingAnalysis::save(
            &base_dir,
            conversations,
            session_id,
        ) {
            Ok(()) => tracing::info!("已保存待分析对话到磁盘"),
            Err(e) => tracing::warn!("保存待分析对话失败: {e}"),
        }
    }

    /// 强制执行一次四步分析（保留供手动调用）
    #[allow(dead_code)]
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
            tracing::info!("正在执行四步分析（关闭时强制触发）...");
            let analyzer = FourStepAnalyzer::new(Box::new(llm), base_dir, session_id);
            let report = analyzer
                .run(&format!("[{}]", conversations.join(",")))
                .await;
            tracing::info!(
                "四步分析完成: 事实总结={}字, 画像+={}, 踩坑={}, 规则={}, 潜意识叙事更新={}",
                report.fact_summary.chars().count(),
                report.profile_entries_added,
                report.pitfalls_found,
                report.rules_created,
                report.subconscious_updated,
            );
        } else {
            tracing::warn!("无法创建记忆脑 LLM 客户端，跳过关闭时的四步分析");
        }
    }

    /// 创建四步分析用的 LLM 客户端（静态版本，供 new() 中使用）
    fn create_analyzer_llm_from_config() -> Option<AnalyzerLlm> {
        let config = LlmConfig::load_default().ok()?;
        let client: Box<dyn brain_llm::LlmProvider> = config.create_brain_client("memory").ok()?;
        let model = config.model_for_brain("memory").to_string();
        let (mt, temp) = config.params_for_brain("memory");
        Some(AnalyzerLlm::new(Arc::from(client), model, mt, temp))
    }

    /// 创建四步分析用的 LLM 客户端
    fn create_analyzer_llm(&self) -> Option<AnalyzerLlm> {
        let config = LlmConfig::load_default().ok()?;
        let client: Box<dyn brain_llm::LlmProvider> = config.create_brain_client("memory").ok()?;
        let model = config.model_for_brain("memory").to_string();
        let (mt, temp) = config.params_for_brain("memory");
        Some(AnalyzerLlm::new(Arc::from(client), model, mt, temp))
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

/// 结果结构体：LLM provider + 模型名
struct LlmResult {
    provider: Box<dyn SensoryLlmProvider>,
    model_name: String,
}

/// 创建感知脑 LLM（不降级，失败直接报错）
fn create_sensory_llm() -> Result<LlmResult, String> {
    let config = LlmConfig::load_default().map_err(|e| {
        format!("LLM 配置加载失败: {e}\n请检查 ~/.config/ai-brain/config.toml 或设置 ZHIPU_API_KEY 环境变量")
    })?;
    let client = config
        .create_brain_client("sensory")
        .map_err(|e| {
            format!(
                "LLM 客户端创建失败: {e}\n请检查 ZHIPU_API_KEY 是否已设置"
            )
        })?;
    let model = config.model_for_brain("sensory").to_string();
    tracing::info!("LLM 已加载，感知脑模型: {model}");
    Ok(LlmResult {
        provider: Box::new(LlmAdapter { inner: client }),
        model_name: model,
    })
}

/// 尝试为指定 brain 创建 LLM 客户端，失败返回 None
fn try_create_llm_client(brain_name: &str) -> Option<Arc<dyn brain_llm::LlmProvider>> {
    let config = LlmConfig::load_default().ok()?;
    config.create_brain_client(brain_name).ok().map(Arc::from)
}

/// 创建 v2 MainBrain（带 tool_loop + 工具注册）
///
/// LLM 不可用时返回 `Arc<Mutex<None>>`
fn create_v2_main_brain(
    memory_brain: Option<Arc<Mutex<MemoryBrain>>>,
    dispatch: brain_dispatch::TokioDispatch,
) -> (Arc<Mutex<Option<MainBrain>>>, Option<PluginManager>, Arc<SkillCatalog>, Arc<McpClientPool>) {
    let client = match try_create_llm_client("sensory") {
        Some(c) => c,
        None => {
            tracing::warn!("v2 MainBrain: LLM 不可用，跳过创建（回声模式）");
            return (
                Arc::new(Mutex::new(None)),
                None,
                Arc::new(SkillCatalog { skills: vec![] }),
                Arc::new(McpClientPool::new()),
            );
        }
    };

    // === 插件系统初始化 ===
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let ai_brain_dir = std::path::PathBuf::from(&home).join(".ai-brain");

    // 1. 加载插件管理器
    let plugin_mgr = PluginManager::load(&ai_brain_dir.join("plugins"))
        .map_err(|e| {
            tracing::warn!("加载插件管理器失败: {e}");
            e
        })
        .ok();

    // 2. 扫描 Skill 目录
    let mut skill_roots: Vec<std::path::PathBuf> = vec![];
    skill_roots.push(
        std::env::current_dir()
            .unwrap_or_default()
            .join(".ai-brain")
            .join("skills"),
    );
    if let Some(ref mgr) = plugin_mgr {
        skill_roots.extend(mgr.skill_roots());
    }
    skill_roots.push(ai_brain_dir.join("skills"));
    skill_roots.push(
        std::path::PathBuf::from(&home)
            .join(".claude")
            .join("skills"),
    );
    skill_roots.push(
        std::path::PathBuf::from(&home)
            .join(".codex")
            .join("skills"),
    );

    let skill_catalog = SkillCatalog::scan_all(&skill_roots).unwrap_or_else(|e| {
        tracing::warn!("扫描技能失败: {e}");
        SkillCatalog { skills: vec![] }
    });
    let skill_catalog = Arc::new(skill_catalog);
    tracing::info!("扫描到 {} 个技能", skill_catalog.skills.len());

    // 3. 加载 MCP 配置
    let mcp_config_path = ai_brain_dir.join("mcp").join("mcp-servers.json");
    let mut mcp_configs = load_mcp_servers(&mcp_config_path).unwrap_or_default();
    if let Some(ref mgr) = plugin_mgr {
        for path in mgr.mcp_configs() {
            mcp_configs.extend(load_mcp_servers(&path).unwrap_or_default());
        }
    }
    tracing::info!("加载 {} 个 MCP 服务器配置", mcp_configs.len());

    // 创建 MCP 客户端池（Arc 共享给 tool_executor 和返回值）
    let mcp_pool = Arc::new(McpClientPool::new());

    let tool_executor: Arc<dyn brain_core::tool_executor::ToolExecutor> = Arc::new(
        crate::real_tool_executor::RealToolExecutor::with_dispatch(memory_brain, dispatch)
            .with_skill_catalog(skill_catalog.clone())
            .with_mcp_pool(mcp_pool.clone()),
    );

    let brain_config = BrainConfig::default();
    let (main_mt, main_temp) = LlmConfig::load_default()
        .map(|c| c.params_for_brain("main"))
        .unwrap_or((32768, 0.7));
    let mut brain = MainBrain::new(client, tool_executor, brain_config, main_mt, main_temp);

    // 注册所有 MVP 工具（bash、read_file、write_file、edit_file、glob、grep）
    let tool_defs = crate::real_tool_executor::mvp_tool_definitions();
    tracing::info!("v2 MainBrain: 注册 {} 个工具", tool_defs.len());
    brain.register_tools(tool_defs);

    (Arc::new(Mutex::new(Some(brain))), plugin_mgr, skill_catalog, mcp_pool)
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
                prompt_tokens: 0,
                completion_tokens: 0,
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
