use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use crate::llm_usage_logger;
use crate::runtime_trace::{ExchangeKind, ExchangePhase, ExchangeStatus, RuntimeExchange};
use crate::web::collaboration_tools::{
    group_message_tool_definition, GroupMessageToolExecutor, GroupMessageToolScope,
};

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

fn general_eval_enabled(config: &HooksConfig) -> bool {
    config.enabled && config.eval_gate.enabled
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub(crate) struct MemberQueryError {
    message: String,
    execution_started: bool,
}

impl MemberQueryError {
    fn before_execution(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            execution_started: false,
        }
    }

    fn after_execution(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            execution_started: true,
        }
    }

    pub(crate) const fn execution_started(&self) -> bool {
        self.execution_started
    }
}

fn resolve_member_reasoning_tokens(configured: u32, depth: &str) -> Result<u32, String> {
    match depth {
        "low" => Ok(configured.min(4_096)),
        "medium" => Ok(configured.min(8_192)),
        "high" => Ok(configured),
        other => Err(format!("不支持的成员思考深度: {other}")),
    }
}

fn resolve_member_model_policy(
    llm_config: &LlmConfig,
    policy_id: &str,
) -> Result<brain_llm::config::ResolvedModelPolicy, MemberQueryError> {
    llm_config
        .available_instance_model_policies()
        .into_iter()
        .find(|policy| policy.policy_id == policy_id)
        .ok_or_else(|| {
            MemberQueryError::before_execution(format!("成员模型策略未配置: {policy_id}"))
        })
}

fn create_member_execution_client(
    llm_config: &LlmConfig,
    policy_id: &str,
) -> Result<
    (
        Box<dyn brain_llm::LlmProvider>,
        brain_llm::config::ResolvedModelPolicy,
    ),
    MemberQueryError,
> {
    let policy = resolve_member_model_policy(llm_config, policy_id)?;
    let client = llm_config
        .create_model_policy_client(policy_id)
        .map_err(|error| {
            MemberQueryError::before_execution(format!("创建成员模型失败: {error}"))
        })?;
    Ok((client, policy))
}

const NOVEL_WRITING_SKILL_NAME: &str = "novel-writing-workflow";
const NOVEL_WRITING_SKILL: &str =
    include_str!("../../brain-main/skills/novel-writing-workflow/SKILL.md");

fn install_builtin_skills(ai_brain_dir: &Path) -> Result<PathBuf, String> {
    let root = ai_brain_dir.join("builtin-skills");
    let skill_dir = root.join(NOVEL_WRITING_SKILL_NAME);
    let skill_path = skill_dir.join("SKILL.md");
    std::fs::create_dir_all(&skill_dir)
        .map_err(|error| format!("创建内置技能目录失败: {error}"))?;
    let current = std::fs::read_to_string(&skill_path).ok();
    if current.as_deref() != Some(NOVEL_WRITING_SKILL) {
        std::fs::write(&skill_path, NOVEL_WRITING_SKILL)
            .map_err(|error| format!("写入内置小说技能失败: {error}"))?;
    }
    Ok(root)
}

use brain_bus::BrainBus;
use brain_core::agent::{BrainAgent, StatelessBrain};
use brain_core::config::BrainConfig;
use brain_core::tool_executor::ToolExecutionContext;
use brain_core::types::{
    BrainId, BrainResponse, BrainResponsePayload, BroadcastMessage, ContextSnapshot,
    EvaluationResult, MainBrainOutput, MasterOutput, MemoryStats, ProgressEvent,
};
use brain_eval::EvalBrain;
use brain_evaluation::EvaluationBrain;
use brain_evolution::{
    BrainRegistry, BrainRegistryStatus, BrainTemplate, CreationSuggestion, SuggestionEngine,
};
use brain_evolver::{
    extract_cycle_metadata, target_id_from_candidate, CycleConfig, CycleRunner, EvoConfig,
    EvolutionCoordinator, EvolutionTrigger, SharedResources,
};
use brain_graph::generic::GenericGraphStore;
use brain_hooks::config::HooksConfig;
use brain_hooks::runner::HookRunner;
use brain_hooks::types::{HookEvent, HookInput};
use brain_llm::{ChatMessage, ChatRequest, LlmConfig};
use brain_main::conversation::ChatMessageRestore;
use brain_main::main_brain::MainBrain;
use brain_master::MasterBrain;
use brain_mcp::config::load_mcp_servers;
use brain_mcp::McpClientPool;
// [Task 16] 旧 analyzer::AnalysisLlm 已随旧模块清理，使用新版 concentration::AnalysisLlm
use brain_memory::conversation_memory::{ConversationMemoryInvalidation, ConversationMemoryScope};
use brain_memory::generic::GenericMemoryStore;
use brain_memory::pyramid_memory_brain::{PyramidMemoryBrain, PyramidMemoryBrainConfig};
use brain_motor::motor_brain::{MotorBrain, MotorConfig};
use brain_plugin::{PluginManager, SkillCatalog};
use brain_reasoning::reasoning_brain::{ReasoningBrain, ReasoningConfig};
use brain_sensory::llm::LlmProvider as SensoryLlmProvider;
use brain_sensory::SensoryBrain;
use brain_validation::validation_brain::ValidationConfig;
use brain_validation::ValidationBrain;
use chrono::Utc;
use knowledge_core::{
    ContentResolverRegistry, ContextBlockKind, ContextBuilder,
    ContextSnapshot as KnowledgeContextSnapshot, GraphQueryPort, GraphQueryRequest,
    GraphQueryResult, KnowledgeError, KnowledgeSchemaBundle, KnowledgeSchemaRegistry,
    MemoryQueryPort, MemoryTypeId, MemoryTypeSchema, NamespaceId, ProjectionAdapterRegistry,
    ResourceTypeId, ScopeTypeId,
};
use novel_application::{
    LegacyNovelImporter, NovelApplicationService, NovelDomainStore, NovelProjectionWorker,
    NovelResourcePort as ApplicationNovelResourcePort, StoreWorkflowEnvironment,
    TaskApplicationPort,
};
use task_engine::{Scheduler, TaskCoordinator, TaskRepository};
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

fn member_inputs_from_snapshot(
    snapshot: &KnowledgeContextSnapshot,
) -> Result<(String, Vec<ChatMessageRestore>, String), String> {
    snapshot
        .validate()
        .map_err(|error| format!("成员上下文快照校验失败: {error}"))?;
    let mut input = None;
    let mut history = Vec::new();
    let mut system_context = Vec::new();
    for block in &snapshot.blocks {
        match block.kind {
            ContextBlockKind::SystemPolicy
            | ContextBlockKind::Memory
            | ContextBlockKind::GraphEvidence
            | ContextBlockKind::Artifact => system_context.push(block.content.clone()),
            ContextBlockKind::ConversationUser => history.push(ChatMessageRestore {
                role: "user".into(),
                content: block.content.clone(),
            }),
            ContextBlockKind::ConversationAssistant => history.push(ChatMessageRestore {
                role: "assistant".into(),
                content: block.content.clone(),
            }),
            ContextBlockKind::ConversationReference => {
                system_context.push(block.content.clone());
            }
            ContextBlockKind::CurrentInput => {
                if input.replace(block.content.clone()).is_some() {
                    return Err("成员上下文包含多个 current_input block".into());
                }
            }
        }
    }
    if !snapshot.degradations.is_empty() {
        system_context.push(
            snapshot
                .degradations
                .iter()
                .map(|item| format!("上下文来源 {} 不完整：{}", item.source, item.reason))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    let input = input.ok_or_else(|| "成员上下文缺少 current_input block".to_string())?;
    if system_context.is_empty() {
        return Err("成员上下文缺少 system_policy block".into());
    }
    Ok((input, history, system_context.join("\n\n")))
}

/// 执行一次已准入的协作成员运行。
///
/// 生产 Orchestrator 与隔离测试服务共享这一执行边界；测试只省略可选的持久记忆写入，
/// MainBrain 隔离 fork、工具包装、工具定义和 tool loop 均走相同实现。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_member_run<F>(
    template: Arc<Mutex<Option<MainBrain>>>,
    memory: Option<Arc<Mutex<PyramidMemoryBrain>>>,
    context_snapshot: KnowledgeContextSnapshot,
    memory_scope: ConversationMemoryScope,
    resolve_model: F,
    allow_tools: bool,
    tool_execution_context: ToolExecutionContext,
    group_message_scope: Option<GroupMessageToolScope>,
    progress_tx: tokio::sync::mpsc::Sender<ProgressEvent>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<MainBrainOutput, MemberQueryError>
where
    F: FnOnce() -> Result<(Arc<dyn brain_llm::LlmProvider>, u32, f64), MemberQueryError>
        + Send
        + 'static,
{
    let (input, restore_history, member_context) = member_inputs_from_snapshot(&context_snapshot)
        .map_err(MemberQueryError::before_execution)?;
    let (client, max_tokens, temperature) = resolve_model()?;

    let mut brain = {
        let template = template.lock().await;
        let template = template.as_ref().ok_or_else(|| {
            MemberQueryError::before_execution("MainBrain 当前不可用，无法创建成员运行")
        })?;
        if allow_tools {
            if let Some(scope) = group_message_scope {
                template.fork_isolated_with_llm_and_executor_in_context(
                    client,
                    Arc::new(GroupMessageToolExecutor::new(
                        template.tool_executor(),
                        scope,
                    )),
                    tool_execution_context,
                    vec![group_message_tool_definition()],
                    max_tokens,
                    temperature,
                )
            } else {
                template.fork_isolated_with_llm_and_executor_in_context(
                    client,
                    template.tool_executor(),
                    tool_execution_context,
                    Vec::new(),
                    max_tokens,
                    temperature,
                )
            }
        } else {
            template.fork_isolated_with_llm_and_executor_in_context(
                client,
                template.tool_executor(),
                tool_execution_context,
                Vec::new(),
                max_tokens,
                temperature,
            )
        }
    };
    if !allow_tools {
        brain.register_tools(Vec::new());
    }
    brain.restore_history(restore_history);
    brain.push_memory_context(&member_context);

    let process = brain.process_input(&input, Some(&progress_tx), Some(cancel));
    let result = crate::query_context::with_conversation_memory_scope(&memory_scope, process)
        .await
        .map_err(|error| MemberQueryError::after_execution(error.to_string()));

    if let (Ok(output), Some(memory)) = (&result, memory) {
        let memory = memory.lock().await;
        if let Err(error) = memory.store_turns_scoped(&output.turns, &memory_scope) {
            tracing::warn!("成员对话存入独立记忆代次失败: {error}");
        }
    }
    result
}

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
    fn new(
        client: Arc<dyn brain_llm::LlmProvider>,
        model: String,
        max_tokens: u32,
        temperature: f64,
    ) -> Self {
        Self {
            client,
            model,
            max_tokens,
            temperature,
        }
    }
}

// [Task 16] 旧 analyzer::AnalysisLlm trait impl 已禁用（analyzer.rs 依赖已删除模块）
// complete() 方法作为固有方法保留（新版 concentration::AnalysisLlm 依赖）
impl AnalyzerLlm {
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

    fn complete_structured(
        &self,
        system: &str,
        user: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        let mut messages = vec![];
        if !system.is_empty() {
            messages.push(ChatMessage::system(system));
        }
        messages.push(ChatMessage::user(user));
        let request = ChatRequest {
            model: Some(self.model.clone()),
            messages,
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

// 同时实现新版浓缩引擎的 AnalysisLlm trait
impl brain_memory::concentration::AnalysisLlm for AnalyzerLlm {
    fn analyze_structured(
        &self,
        system: &str,
        user: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        self.complete_structured(system, user)
    }
}

// ─── 主脑状态 ────────────────────────────────────────────────────

struct MasterState {
    master: MasterBrain,
    broadcast_rx: brain_bus::BroadcastReceiver,
    result_rx: brain_bus::ResultReceiver,
}

struct UnavailableGraphQuery {
    reason: String,
}

impl GraphQueryPort for UnavailableGraphQuery {
    fn query(&self, _query: &GraphQueryRequest) -> knowledge_core::Result<GraphQueryResult> {
        Err(KnowledgeError::Unavailable(self.reason.clone()))
    }
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
    memory_brain: Arc<Mutex<PyramidMemoryBrain>>,
    context_builder: Arc<ContextBuilder>,
    #[allow(dead_code)]
    projection_adapters: Arc<ProjectionAdapterRegistry>,
    evaluation_brain: EvaluationBrain,
    /// v2 评估脑（LLM 深度评估），None 表示 LLM 不可用
    eval_brain: Option<EvalBrain>,
    /// Hook 执行引擎（eval_gate 决策等）
    hook_runner: HookRunner,
    registry: Arc<Mutex<BrainRegistry>>,
    suggestion_engine: Arc<Mutex<SuggestionEngine>>,
    /// v2 进化脑: 调度器（目标选择 + 日志）
    evo_coordinator: Arc<Mutex<Option<EvolutionCoordinator>>>,
    /// v2 进化脑: 空闲触发检测器
    evo_trigger: Arc<Mutex<EvolutionTrigger>>,
    /// v2 进化脑: 后台进化任务句柄（用于 cancel）
    evo_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// v2 进化脑: 是否正在进化中（防重入）
    evo_running: Arc<tokio::sync::watch::Sender<bool>>,
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
    /// Full runtime exchanges for WebSocket cockpit subscribers.
    runtime_trace_tx: broadcast::Sender<RuntimeExchange>,
    /// Shared durable execution repository for Collaboration and domain workflows.
    task_repository: Arc<TaskRepository>,
    /// One admission boundary shared by every runtime workflow.
    task_coordinator: TaskCoordinator,
    /// High-level Novel task application boundary.
    novel_application: Arc<dyn TaskApplicationPort>,
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
}

impl Orchestrator {
    /// 初始化所有组件并启动副脑任务
    pub async fn new() -> Result<Self, String> {
        // 0. 统一加载 LLM 配置（后续全部复用，不再重复读盘）
        let llm_config = LlmConfig::load_default().map_err(|e| {
            format!("LLM 配置加载失败: {e}\n请检查 ~/.config/ai-brain/config.toml 或设置 ZHIPU_API_KEY 环境变量")
        })?;
        let runtime_dir = crate::web::collaboration::default_runtime_dir();
        Self::new_with_runtime(llm_config, runtime_dir).await
    }

    async fn new_with_runtime(llm_config: LlmConfig, runtime_dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&runtime_dir)
            .map_err(|error| format!("创建运行时目录失败: {error}"))?;
        let direct_working_directory =
            std::env::current_dir().map_err(|error| format!("无法确定主脑工作目录: {error}"))?;
        let direct_tool_execution_context =
            crate::command_execution::tool_execution_context_for_directory(
                &direct_working_directory,
            )?;
        let collaboration_config =
            crate::web::collaboration::CollaborationConfig::load(&runtime_dir.join("config.toml"))
                .map_err(|error| format!("加载统一执行配置失败: {error}"))?;
        let task_database_path = runtime_dir.join("runtime.db");
        let task_repository = Arc::new(
            tokio::task::spawn_blocking(move || TaskRepository::open(task_database_path))
                .await
                .map_err(|error| format!("初始化 TaskEngine 线程失败: {error}"))?
                .map_err(|error| format!("初始化 TaskEngine 失败: {error}"))?,
        );
        let recovery_repository = Arc::clone(&task_repository);
        let recovery = tokio::task::spawn_blocking(move || recovery_repository.recover_inflight())
            .await
            .map_err(|error| format!("恢复 TaskEngine 线程失败: {error}"))?
            .map_err(|error| format!("恢复 TaskEngine 失败: {error}"))?;
        if recovery.interrupted > 0 {
            tracing::warn!(
                interrupted = recovery.interrupted,
                requeued = recovery.requeued,
                needs_input = recovery.needs_input,
                "已恢复中断的 TaskEngine 节点"
            );
        }
        let scheduler = Scheduler::new(collaboration_config.scheduler_limits())
            .map_err(|error| format!("初始化统一 Scheduler 失败: {error}"))?;
        let task_coordinator = TaskCoordinator::new(Arc::clone(&task_repository), scheduler);

        // 1. 三通道消息总线
        let bus = Arc::new(BrainBus::new(64, 64, 64));

        // 2. 感知脑（LLM 不可用直接报错，不降级）
        let sensory_llm_result = create_sensory_llm(&llm_config)?;
        let model_name = sensory_llm_result.model_name.clone();
        let sensory = SensoryBrain::new(
            &sensory_llm_result.model_name,
            bus.clone(),
            sensory_llm_result.provider,
        );

        // 3. 创建各副脑
        let (memory, mut reasoning, mut motor, validation, evaluation) = create_sub_brains()?;

        // 4. 注入 LLM 到需要慢思考的副脑
        if let Ok(client) = llm_config.create_brain_client("reasoning") {
            let llm: Arc<dyn brain_llm::LlmProvider> = Arc::from(client);
            reasoning.set_llm(llm.clone());
            motor.set_llm(llm);
            tracing::info!("推理脑+执行脑已注入 LLM");
        }
        // 记忆脑不再需要 set_llm（浓缩引擎在需要时动态创建 LLM）

        // 4.0 提前包装记忆脑 Arc<Mutex>（评估脑和后续都需要）
        let memory = Arc::new(Mutex::new(memory));

        // 4.1 先读取 Hook 配置；通用评估脑默认关闭，仅显式启用时才创建 LLM。
        let hooks_config: HooksConfig = llm_config
            .hooks
            .as_ref()
            .map(HooksConfig::from_toml_value)
            .unwrap_or_default();
        let eval_enabled = general_eval_enabled(&hooks_config);

        let eval_tool_executor: Arc<dyn brain_core::tool_executor::ToolExecutor> = Arc::new(
            crate::real_tool_executor::RealToolExecutor::with_memory(Some(memory.clone())),
        );
        let mut eval_brain = if eval_enabled {
            if let Ok(client) = llm_config.create_brain_client("eval") {
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
            let skills_dir = std::path::Path::new("rust/crates/brain-eval/skills");
            if let Err(e) = eb.load_skills_from_dir(skills_dir) {
                tracing::warn!("加载评估脑 skills 失败: {e}");
            } else {
                tracing::info!("评估脑 skills 加载成功");
            }
        }
        if eval_brain.is_some() {
            tracing::info!("v2 评估脑已创建（LLM 深度评估 + 只读工具验证）");
        } else if !eval_enabled {
            tracing::info!("v2 通用评估脑默认关闭；小说任务由小说脑自检和主脑复审");
        }

        // 4.2 初始化 Hook 系统（eval_gate 显式启用后才注册）
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

        // 注意：记忆脑不再作为独立 BrainAgent 运行循环，只在其他副脑的 slow_think 中被调用
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

        // 11.1 进化脑 coordinator（惰性初始化，首次 /evo 时创建）
        let evo_coordinator_arc: Arc<Mutex<Option<EvolutionCoordinator>>> =
            Arc::new(Mutex::new(None));

        tracing::info!("AI Brain 初始化完成，{} 个副脑任务已启动", tasks.len());

        // model_name 已从 create_sensory_llm 获取，此处不再重复计算

        // 11.5 初始化消息调度中间件（必须在 v2_brain 之前，因为 dispatch 要注入 RealToolExecutor）
        let dispatch = brain_dispatch::TokioDispatch::new(256);
        let (dispatch_output_tx, dispatch_output_rx) =
            tokio::sync::mpsc::channel::<brain_dispatch::MainLoopMessage>(64);
        let (runtime_trace_tx, _) = broadcast::channel::<RuntimeExchange>(256);

        // Novel Domain owns its durable database; legacy Pyramid files are read-only inputs.
        let novel_base_dir = memory.lock().await.base_dir().to_path_buf();
        let novel_store = Arc::new(
            NovelDomainStore::open(novel_base_dir.join("novel.db"))
                .map_err(|error| format!("初始化 Novel 领域库失败: {error}"))?,
        );
        let migration = LegacyNovelImporter::new(&novel_base_dir)
            .import_into(&novel_store)
            .map_err(|error| format!("迁移旧 Novel 数据失败: {error}"))?;
        tracing::info!(
            projects = migration.projects_imported,
            checkpoints = migration.checkpoints_imported,
            events = migration.events_imported,
            publications = migration.publications_imported,
            archived = migration.tasks_archived,
            "Novel 数据切流检查完成"
        );
        let workspace_root =
            std::env::current_dir().map_err(|error| format!("无法确定小说项目工作区: {error}"))?;
        let novel_resources = Arc::new(
            crate::novel_adapters::ScopedNovelResourceAdapter::new(&workspace_root)
                .map_err(|error| format!("初始化小说资源端口失败: {error}"))?,
        );
        let application_resources: Arc<dyn ApplicationNovelResourcePort> = novel_resources;
        let novel_writer = create_novel_writer_client(&llm_config)?;
        let novel_start_workflow = {
            let model = novel_writer.model;
            let models = novel_workflow::NovelWorkflowModels {
                writer: model.clone(),
                reviewer: model.clone(),
                canon_extractor: model,
            };
            let budget = novel_workflow::NovelWorkflowBudget {
                input_tokens: collaboration_config.task_input_token_limit,
                output_tokens: collaboration_config
                    .task_output_token_limit
                    .min(u64::from(novel_writer.max_output_tokens)),
            };
            let environment = Arc::new(StoreWorkflowEnvironment::new(
                Arc::clone(&novel_store),
                Arc::clone(&application_resources),
            ));
            let writer = Arc::new(crate::novel_adapters::LlmNovelWriterAdapter::new(
                novel_writer.provider,
                novel_writer.temperature,
            ));
            Some(Arc::new(novel_workflow::NovelStartWorkflow::new(
                Arc::clone(&task_repository),
                task_coordinator.clone(),
                environment,
                writer,
                models,
                budget,
            )))
        };
        let novel_application = Arc::new(NovelApplicationService::new(
            Arc::clone(&novel_store),
            novel_start_workflow,
            application_resources,
        ));
        // Novel 应用仍由 Orchestrator 保存，主脑工具执行器的注入入口在下方保持停用。
        let novel_application_port: Arc<dyn TaskApplicationPort> = novel_application.clone();

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
        let (v2_brain, plugin_mgr, skill_catalog, mcp_pool) = create_v2_main_brain(
            &llm_config,
            Some(Arc::clone(&memory)),
            dispatch.clone(),
            runtime_trace_tx.clone(),
            direct_tool_execution_context,
            // Arc::clone(&novel_application_port),
        );

        // 12.0 评估脑接入统一 SkillCatalog
        if let Some(ref mut eb) = eval_brain {
            eb.set_skill_catalog(Arc::clone(&skill_catalog));
            tracing::info!(
                "评估脑已接入统一 SkillCatalog（{} 个技能）",
                skill_catalog.skills.len()
            );
        }

        // 12.05 Bootstrap 技能注入到主脑
        if !skill_catalog.bootstrap_skills.is_empty() {
            if let Ok(mut v2_guard) = v2_brain.try_lock() {
                if let Some(ref mut brain) = *v2_guard {
                    for bs in &skill_catalog.bootstrap_skills {
                        match skill_catalog.load_content(bs) {
                            Ok(content) => {
                                brain.inject_bootstrap(content);
                                tracing::info!("Bootstrap 技能 '{}' 已注入主脑", bs.name);
                            }
                            Err(e) => {
                                tracing::warn!("加载 bootstrap 技能 '{}' 失败: {e}", bs.name);
                            }
                        }
                    }
                }
            }
        }

        // 12.1 [Task 16] 旧守护线程已禁用（guardian.rs 已删除，由金字塔浓缩引擎替代）
        // {
        //     let mem_base_dir = {
        //         let mem_guard = memory.lock().await;
        //         mem_guard.base_dir().to_path_buf()
        //     };
        //     let guardian_llm: Option<Box<dyn AnalysisLlm>> =
        //         if let Ok(config) = LlmConfig::load_default() {
        //             if let Ok(client) = config.create_brain_client("memory") {
        //                 let (mt, temp) = config.params_for_brain("memory");
        //                 Some(Box::new(AnalyzerLlm::new(
        //                     Arc::from(client),
        //                     config.model_for_brain("memory").to_string(),
        //                     mt,
        //                     temp,
        //                 )))
        //             } else {
        //                 None
        //             }
        //         } else {
        //             None
        //         };
        //
        //     if let Some(llm) = guardian_llm {
        //         let mut guardian_config = brain_memory::guardian::GuardianConfig::default();
        //         guardian_config.check_interval_secs = 28800; // 8h
        //         guardian_config.min_new_summaries = 10;
        //         let engine =
        //             brain_memory::guardian::GuardianEngine::new(mem_base_dir, guardian_config, llm);
        //         let mut shutdown_rx_g = shutdown_rx.clone();
        //         let interval = tokio::time::Duration::from_secs(28800);
        //         let guardian_handle = tokio::spawn(async move {
        //             loop {
        //                 tokio::select! {
        //                     _ = tokio::time::sleep(interval) => {
        //                         match engine.should_run() {
        //                             Ok(true) => {
        //                                 match engine.run().await {
        //                                     Ok(report) => {
        //                                         if report.archived_count > 0 || report.new_topics > 0 {
        //                                             tracing::info!(
        //                                                 "守护线程: 归档{}条, 新建{}主题, 合并{}主题",
        //                                                 report.archived_count,
        //                                                 report.new_topics,
        //                                                 report.merged_topics,
        //                                             );
        //                                         }
        //                                     }
        //                                     Err(e) => tracing::warn!("守护线程运行失败: {e}"),
        //                                 }
        //                             }
        //                             Ok(false) => {} // 条件不满足，下次再检查
        //                             Err(e) => tracing::warn!("守护线程检查失败: {e}"),
        //                         }
        //                     }
        //                     _ = shutdown_rx_g.changed() => {
        //                         tracing::info!("守护线程收到关机信号");
        //                         break;
        //                     }
        //                 }
        //             }
        //         });
        //         tasks.push(guardian_handle);
        //         tracing::info!("守护线程已启动 (8h检查间隔)");
        //     }
        // }

        // 12.5 启动时注入金字塔记忆上下文（潜意识 + 画像 + 可注入经验 + 人格 prompt）
        let injection_text = if let Ok(mem_guard) = memory.try_lock() {
            let pp = mem_guard.persona_manager().build_persona_prompt();
            let inject_ctx = match mem_guard.auto_inject() {
                Ok(ctx) => ctx,
                Err(e) => {
                    tracing::warn!("自动注入失败: {e}");
                    drop(mem_guard);
                    return Err(format!("记忆注入失败: {e}"));
                }
            };

            let mut parts = Vec::new();
            if !pp.is_empty() {
                parts.push(pp);
            }
            if !inject_ctx.subconscious_text.is_empty() {
                parts.push(format!(
                    "[潜意识印象 — 你曾经做过这些事，匹配到时再深入回忆]\n{}",
                    inject_ctx.subconscious_text
                ));
            }
            if !inject_ctx.profile.is_empty() {
                parts.push(format!("[用户画像] {}", inject_ctx.profile));
            }
            if !inject_ctx.injectable_experiences.is_empty() {
                let exp_lines: Vec<String> = inject_ctx
                    .injectable_experiences
                    .iter()
                    .map(|e| format!("- {}", e.description))
                    .collect();
                parts.push(format!("[经验规则]\n{}", exp_lines.join("\n")));
            }
            drop(mem_guard);
            parts.join("\n\n")
        } else {
            tracing::warn!("记忆脑锁被占用，跳过启动注入");
            String::new()
        };
        if !injection_text.is_empty() {
            if let Ok(mut v2_guard) = v2_brain.try_lock() {
                if let Some(ref mut brain) = *v2_guard {
                    brain.inject_memory_context(&injection_text);
                    tracing::info!("金字塔记忆上下文已注入主脑");
                }
            }
        }

        // 12.6 检查并注入上次会话的待分析对话
        let (pending_base_dir, memory_is_stale) = {
            let mem = match memory.try_lock() {
                Ok(m) => m,
                Err(_) => return Err("记忆脑锁被占用".to_string()),
            };
            (
                mem.base_dir().to_path_buf(),
                mem.conversation_memory_is_stale(),
            )
        };
        let pending = (!memory_is_stale)
            .then(|| brain_memory::pending_analysis::PendingAnalysis::load(&pending_base_dir))
            .flatten();
        if let Some(pending) = pending {
            let injection_text = pending.format_for_injection();
            let convs_for_analysis = pending.conversations.clone();
            let sess_id = pending.session_id.clone();
            let base_for_analysis = pending_base_dir.clone();

            // 1) 注入主脑上下文（LLM 立即可用）
            if let Ok(mut v2_guard) = v2_brain.try_lock() {
                if let Some(ref mut brain) = *v2_guard {
                    brain.push_memory_context(&injection_text);
                    tracing::info!("已注入上次会话记忆 ({}条对话)", convs_for_analysis.len());
                }
            }

            // 2) 后台跑四步浓缩（更新金字塔记忆，fire-and-forget）
            let llm = Self::create_analyzer_llm_with_config(&llm_config);
            drop(tokio::spawn(async move {
                if let Some(llm) = llm {
                    let graph_db_path = Some(base_for_analysis.join("graph").join("graph.db"));
                    let config = PyramidMemoryBrainConfig {
                        base_dir: base_for_analysis,
                        session_id: sess_id,
                        graph_db_path,
                    };
                    if let Ok(brain) = PyramidMemoryBrain::new(config) {
                        let report = brain.concentrate(&llm).await;
                        tracing::info!(
                            "后台四步浓缩完成: tasks={}, types={}, triggers={}, narrative={}字, profile_updated={}, errors={}",
                            report.step1_tasks, report.step2_types, report.step3_triggers,
                            report.step3_narrative_chars, report.step4_profile_updated, report.errors.len(),
                        );
                    }
                }
            }));
        }

        let (context_builder, projection_adapters, generic_memory, generic_graph) =
            create_knowledge_runtime(&pending_base_dir)?;
        if let Some(graph) = generic_graph {
            let worker = Arc::new(NovelProjectionWorker::new(
                Arc::clone(&novel_store),
                generic_memory,
                graph,
            ));
            let projection = worker
                .drain(1_024)
                .map_err(|error| format!("重建 Novel 通用知识失败: {error}"))?;
            novel_application.attach_projection_worker(worker);
            tracing::info!(
                events = projection.completed_events,
                memories = projection.memory_entries,
                graph_batches = projection.graph_batches,
                "Novel 通用知识 outbox 已收敛"
            );
        }

        Ok(Self {
            bus,
            sensory,
            master_state,
            memory_brain: memory,
            context_builder,
            projection_adapters,
            evaluation_brain: evaluation,
            eval_brain,
            hook_runner,
            registry,
            suggestion_engine,
            evo_coordinator: evo_coordinator_arc,
            evo_trigger: Arc::new(Mutex::new(EvolutionTrigger::new(Default::default()))),
            evo_handle: Arc::new(Mutex::new(None)),
            evo_running: Arc::new(tokio::sync::watch::channel(false).0),
            tasks,
            shutdown_tx,
            query_count: std::sync::atomic::AtomicU32::new(0),
            model_name: model_name.to_string(),
            v2_brain,
            dispatch,
            dispatch_output_rx: Arc::new(Mutex::new(dispatch_output_rx)),
            runtime_trace_tx,
            task_repository,
            task_coordinator,
            novel_application: novel_application_port,
            plugin_mgr,
            skill_catalog,
            mcp_pool,
        })
    }

    pub(crate) fn task_repository(&self) -> Arc<TaskRepository> {
        Arc::clone(&self.task_repository)
    }

    pub(crate) fn task_coordinator(&self) -> TaskCoordinator {
        self.task_coordinator.clone()
    }

    /// 提交查询
    pub async fn query(&self, input: &str) -> Result<MasterOutput, String> {
        // 通知进化脑有用户活动（重置空闲计时器）
        self.touch_evo_activity().await;

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

        // 旧版上下文健康度诊断也服从通用评估脑开关；手动 evaluate 接口仍可用。
        if self.eval_brain.is_some() && self.evaluation_brain.should_evaluate(&snapshots, 0, true) {
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

    /// 获取插件管理器（可变引用，用于 install/uninstall）
    pub fn plugin_mgr_mut(&mut self) -> Option<&mut PluginManager> {
        self.plugin_mgr.as_mut()
    }

    /// 获取技能目录引用
    pub fn skill_catalog(&self) -> &SkillCatalog {
        &self.skill_catalog
    }

    /// 获取 MCP 客户端池引用
    pub fn mcp_pool(&self) -> &McpClientPool {
        &self.mcp_pool
    }

    /// 获取记忆脑 Arc（供 TUI 命令系统使用）
    pub fn memory_brain(&self) -> Arc<Mutex<PyramidMemoryBrain>> {
        Arc::clone(&self.memory_brain)
    }

    pub(crate) fn context_builder(&self) -> Arc<ContextBuilder> {
        Arc::clone(&self.context_builder)
    }

    pub fn subscribe_runtime_trace(&self) -> broadcast::Receiver<RuntimeExchange> {
        self.runtime_trace_tx.subscribe()
    }

    /// 恢复会话历史到 MainBrain（Web 重启/会话切换后使用）
    pub async fn restore_session_history(&self, msgs: Vec<ChatMessageRestore>) {
        let mut guard = self.v2_brain.lock().await;
        if let Some(ref mut brain) = *guard {
            brain.restore_history(msgs);
        }
    }

    /// Invalidate memory written by superseded Web generations and immediately
    /// remove derived memory from MainBrain's system context. Persona-authored
    /// instructions remain active; Novel Canon/lifecycle storage is untouched.
    pub async fn invalidate_conversation_memory(
        &self,
        invalidation: ConversationMemoryInvalidation,
    ) -> Result<(), String> {
        let conversation_id = invalidation.conversation_id.clone();
        let generation_ids = invalidation.generation_ids.clone();
        let includes_legacy_unscoped = invalidation.includes_legacy_unscoped;
        let persona_prompt = {
            let memory = self.memory_brain.lock().await;
            memory
                .invalidate_conversation_memory(&invalidation)
                .map_err(|error| format!("失效旧分支记忆失败: {error}"))?;
            memory.persona_manager().build_persona_prompt()
        };

        let cancelled_tasks = self
            .novel_application
            .invalidate_conversation_generations(
                &conversation_id,
                &generation_ids,
                includes_legacy_unscoped,
            )
            .await
            .map_err(|error| format!("失效旧分支小说任务失败: {error}"))?;
        if !cancelled_tasks.is_empty() {
            tracing::info!(
                "会话分叉已取消未发布小说任务: {}",
                cancelled_tasks.join(", ")
            );
        }

        let mut guard = self.v2_brain.lock().await;
        if let Some(ref mut brain) = *guard {
            brain.replace_memory_context(
                (!persona_prompt.trim().is_empty()).then_some(persona_prompt),
            );
        }
        Ok(())
    }

    /// 系统状态结构体（TUI 用）
    pub fn status_structured(&self) -> SystemStatus {
        let (context_usage, cumulative_prompt, cumulative_completion, cumulative_cache_read) = self
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
                    )
                })
            })
            .unwrap_or((0.0, 0, 0, 0));
        SystemStatus {
            model: self.model_name.clone(),
            query_count: self.query_count.load(std::sync::atomic::Ordering::Relaxed),
            eval_enabled: self.eval_brain.is_some(),
            context_usage,
            cumulative_prompt_tokens: cumulative_prompt,
            cumulative_completion_tokens: cumulative_completion,
            cumulative_cache_read_tokens: cumulative_cache_read,
        }
    }

    /// 获取补全数据（模板名 + 模式关键词），供 TUI 补全使用
    pub fn completion_data(&self) -> (Vec<String>, Vec<String>) {
        let template_names = match self.registry.try_lock() {
            Ok(guard) => guard
                .list_templates()
                .iter()
                .map(|t| t.name.clone())
                .collect(),
            Err(_) => Vec::new(),
        };
        let pattern_keywords = match self.suggestion_engine.try_lock() {
            Ok(guard) => guard
                .patterns()
                .iter()
                .flat_map(|p| p.keywords.clone())
                .collect(),
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
        self.query_streaming_with_memory_scope(input, None)
    }

    /// Web query variant that tags every persisted L1 turn with the current
    /// user-message generation.
    pub fn query_streaming_scoped(
        self: &Arc<Self>,
        input: &str,
        memory_scope: ConversationMemoryScope,
    ) -> (
        tokio::sync::mpsc::Receiver<ProgressEvent>,
        tokio::task::JoinHandle<Result<MainBrainOutput, String>>,
        tokio_util::sync::CancellationToken,
    ) {
        self.query_streaming_with_memory_scope(input, Some(memory_scope))
    }

    /// Execute one collaboration-member run with a fresh MainBrain history.
    ///
    /// Provider/tool infrastructure is shared, while conversation state and
    /// usage counters are isolated for this run. All model-visible content is
    /// derived from the one frozen snapshot persisted by TaskEngine.
    pub(crate) fn query_member_streaming_scoped(
        self: &Arc<Self>,
        context_snapshot: KnowledgeContextSnapshot,
        memory_scope: ConversationMemoryScope,
        llm_config: Arc<LlmConfig>,
        model_policy: &str,
        reasoning_depth: &str,
        allow_tools: bool,
        tool_execution_context: ToolExecutionContext,
        group_message_scope: Option<GroupMessageToolScope>,
    ) -> (
        tokio::sync::mpsc::Receiver<ProgressEvent>,
        tokio::task::JoinHandle<Result<MainBrainOutput, MemberQueryError>>,
        tokio_util::sync::CancellationToken,
    ) {
        self.query_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let (tx, rx) = tokio::sync::mpsc::channel(256);
        let template = Arc::clone(&self.v2_brain);
        let memory = Some(Arc::clone(&self.memory_brain));
        let model_policy = model_policy.to_string();
        let reasoning_depth = reasoning_depth.to_string();
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_for_run = cancel.clone();

        let handle = tokio::spawn(execute_member_run(
            template,
            memory,
            context_snapshot,
            memory_scope,
            move || {
                let (client, resolved_model_policy) =
                    create_member_execution_client(&llm_config, &model_policy)?;
                let max_tokens = resolve_member_reasoning_tokens(
                    resolved_model_policy.max_output_tokens,
                    &reasoning_depth,
                )
                .map_err(MemberQueryError::before_execution)?;
                Ok((
                    Arc::from(client),
                    max_tokens,
                    resolved_model_policy.temperature,
                ))
            },
            allow_tools,
            tool_execution_context,
            group_message_scope,
            tx,
            cancel_for_run,
        ));

        (rx, handle, cancel)
    }

    fn query_streaming_with_memory_scope(
        self: &Arc<Self>,
        input: &str,
        memory_scope: Option<ConversationMemoryScope>,
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

                    let memory_exchange_id = format!("memory-{}", Uuid::new_v4());
                    let _ = this.runtime_trace_tx.send(RuntimeExchange::new(
                        &memory_exchange_id,
                        "main",
                        "主脑",
                        "memory",
                        "记忆脑",
                        ExchangeKind::Memory,
                        ExchangePhase::Request,
                        "召回任务相关记忆",
                        &input_owned,
                        ExchangeStatus::Running,
                        None,
                    ));
                    let recalled_memories = {
                        let mem = this.memory_brain.lock().await;
                        mem.recall_for_context(&input_owned, 3)
                    };
                    let memory_context = recalled_memories
                        .iter()
                        .map(|entry| entry.content.trim())
                        .filter(|content| !content.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if memory_context.is_empty() {
                        let _ = this.runtime_trace_tx.send(RuntimeExchange::new(
                            &memory_exchange_id,
                            "memory",
                            "记忆脑",
                            "main",
                            "主脑",
                            ExchangeKind::Memory,
                            ExchangePhase::Response,
                            "未找到相关记忆",
                            "本次召回没有返回可注入的历史上下文。",
                            ExchangeStatus::Empty,
                            None,
                        ));
                    } else {
                        let memory_count = recalled_memories.len();
                        let preview = truncate_chars(&memory_context, 120).to_string();
                        let _ = this.runtime_trace_tx.send(RuntimeExchange::new(
                            &memory_exchange_id,
                            "memory",
                            "记忆脑",
                            "main",
                            "主脑",
                            ExchangeKind::Memory,
                            ExchangePhase::Response,
                            "返回相关记忆上下文",
                            &memory_context,
                            ExchangeStatus::Completed,
                            None,
                        ));
                        brain.push_memory_context(&memory_context);
                        let _ = tx
                            .send(ProgressEvent::MemoryInjected {
                                count: memory_count,
                                preview,
                            })
                            .await;
                        let _ = tx
                            .send(ProgressEvent::IntermediateConclusion {
                                brain: "main".into(),
                                content: "已了解相关历史上下文，现在继续分析当前任务。".into(),
                            })
                            .await;
                    }

                    // --- 1. 主脑首次处理 ---
                    let process =
                        brain.process_input(&input_owned, Some(&tx), Some(cancel_clone.clone()));
                    let mut result = match &memory_scope {
                        Some(scope) => {
                            crate::query_context::with_conversation_memory_scope(scope, process)
                                .await
                        }
                        None => process.await,
                    }
                    .map_err(|e| format!("{e}"));

                    // 将本轮对话完整轨迹存入记忆脑（L1 全量基座）
                    if let Ok(ref output) = result {
                        let mem = this.memory_brain.lock().await;
                        let stored = match &memory_scope {
                            Some(scope) => mem.store_turns_scoped(&output.turns, scope),
                            None => mem.store_turns(&output.turns),
                        };
                        if let Err(e) = stored {
                            tracing::warn!("v2 对话存入记忆脑失败: {e}");
                        }
                    }

                    // --- 2. Hook 系统决策是否触发评估 ---
                    let ai_answer = result
                        .as_ref()
                        .ok()
                        .map(|o| o.answer.clone())
                        .unwrap_or_default();

                    // 提取 turns 中的工具名列表，供 eval_gate 判断文件修改
                    let tool_names_csv = result
                        .as_ref()
                        .ok()
                        .map(|o| {
                            o.turns
                                .iter()
                                .filter(|t| matches!(t.role, brain_core::types::TurnRole::ToolCall))
                                .filter_map(|t| t.tool_call.as_ref().map(|tc| tc.tool_name.clone()))
                                .collect::<Vec<_>>()
                                .join(",")
                        })
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
                        ai_output: Some(if tool_names_csv.is_empty() {
                            ai_answer
                        } else {
                            // 将工具名列表注入 ai_output，供 eval_gate builtin 解析
                            // 格式: "__tool_names:Edit,Write,Bash__\n{actual_answer}"
                            format!("__tool_names:{}__\n{}", tool_names_csv, ai_answer)
                        }),
                    };
                    let hook_outputs = this.hook_runner.run(&hook_input).await;
                    let should_eval = hook_outputs.iter().any(|o| o.trigger_eval);

                    // --- 3. 评估反馈循环 ---
                    if should_eval && result.is_ok() {
                        tracing::info!("eval_gate 判定：需要评估");
                        if let Some(ref eb) = this.eval_brain {
                            // 从金字塔记忆加载评估信息（requirements + profile + pitfalls）
                            let (eval_requirements, profile_summary, pitfall_descriptions) = {
                                let mem = this.memory_brain.lock().await;
                                let reqs = mem.load_eval_requirements();
                                let profile = mem.load_profile_summary().ok();
                                let pitfalls = mem.load_active_pitfalls();
                                let pitfall_descs: Vec<String> =
                                    pitfalls.iter().map(|p| p.description.clone()).collect();
                                (reqs, profile, pitfall_descs)
                            };

                            tracing::info!(
                                "v2 评估脑开始评估 (用户评估要求={})",
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

                                let eval_exchange_id = format!("evaluation-{}", Uuid::new_v4());
                                let _ = this.runtime_trace_tx.send(RuntimeExchange::new(
                                    &eval_exchange_id,
                                    "main",
                                    "主脑",
                                    "eval",
                                    "评估脑",
                                    ExchangeKind::Evaluation,
                                    ExchangePhase::Request,
                                    format!("第 {} 次回答评估", attempt + 1),
                                    format!(
                                        "用户任务:\n{}\n\n待评估回答:\n{}",
                                        input_owned, answer
                                    ),
                                    ExchangeStatus::Running,
                                    None,
                                ));
                                let _ = tx.send(ProgressEvent::EvaluationStart).await;
                                let _ = tx.send(ProgressEvent::Evaluating).await;
                                let _ = tx
                                    .send(ProgressEvent::IntermediateConclusion {
                                        brain: "main".into(),
                                        content: "已形成阶段性回答，正在交给评估脑核验。".into(),
                                    })
                                    .await;

                                match eb
                                    .evaluate(
                                        &input_owned,
                                        &answer,
                                        &result.as_ref().unwrap().turns,
                                        &eval_requirements,
                                        profile_summary.as_deref(),
                                        Some(&pitfall_descriptions),
                                    )
                                    .await
                                {
                                    Ok(eval_result) => {
                                        let _ = this.runtime_trace_tx.send(RuntimeExchange::new(
                                            &eval_exchange_id,
                                            "eval",
                                            "评估脑",
                                            "main",
                                            "主脑",
                                            ExchangeKind::Evaluation,
                                            ExchangePhase::Response,
                                            if eval_result.passed {
                                                "评估通过"
                                            } else {
                                                "评估反馈"
                                            },
                                            &eval_result.feedback,
                                            if eval_result.passed {
                                                ExchangeStatus::Completed
                                            } else {
                                                ExchangeStatus::Failed
                                            },
                                            None,
                                        ));
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
                                        let _ = tx
                                            .send(ProgressEvent::IntermediateConclusion {
                                                brain: "main".into(),
                                                content: if eval_result.passed {
                                                    "评估已通过，正在整理最终回复。".into()
                                                } else {
                                                    "评估发现需要调整的内容，主脑将继续修正。"
                                                        .into()
                                                },
                                            })
                                            .await;

                                        if eval_result.passed {
                                            tracing::info!("v2 评估通过 (第{}次)", attempt + 1);
                                            break;
                                        }

                                        // 判断严重程度：检查反馈中是否包含 [Critical] 标记
                                        let has_critical =
                                            eval_result.feedback.contains("[Critical]");

                                        if has_critical {
                                            // Critical: 严重问题（事实性错误/结论被证伪/违反禁忌）
                                            // 注入反馈让主脑看到，但不自动重试，让用户决定
                                            tracing::warn!(
                                                "v2 评估发现 Critical 问题(第{}次): {}",
                                                attempt + 1,
                                                truncate_chars(&eval_result.feedback, 300)
                                            );
                                            brain.push_evaluator_to_history(&eval_result.feedback);

                                            // Task 18: 写入 evolution backlog
                                            let _ = this
                                                .add_backlog_entry(
                                                    "Eval",
                                                    "KnowledgeGap",
                                                    &truncate_chars(&eval_result.feedback, 500),
                                                    "Critical",
                                                    None,
                                                )
                                                .await;

                                            break; // 不重试，直接输出当前结果 + 评估反馈
                                        }

                                        if attempt >= max_eval_retries {
                                            tracing::warn!(
                                                "v2 评估达到最大重试次数({}), 使用当前输出",
                                                max_eval_retries + 1
                                            );

                                            // Task 18: 写入 evolution backlog
                                            let _ = this
                                                .add_backlog_entry(
                                                    "Eval",
                                                    "ReasoningWeakness",
                                                    &truncate_chars(&eval_result.feedback, 500),
                                                    "High",
                                                    None,
                                                )
                                                .await;

                                            break;
                                        }

                                        // Warning: 非严重问题，自动注入历史重试
                                        tracing::warn!(
                                            "v2 评估发现 Warning 问题(第{}次): {}",
                                            attempt + 1,
                                            truncate_chars(&eval_result.feedback, 200)
                                        );

                                        // 将评估反馈注入主脑对话历史（Evaluator 角色）
                                        brain.push_evaluator_to_history(&eval_result.feedback);

                                        // 主脑根据反馈重新生成
                                        let revision_prompt =
                                            "请根据以上评估反馈修正你的回答，直接输出修正后的完整内容。";
                                        let retry_process =
                                            brain.process_input(revision_prompt, Some(&tx), None);
                                        let retry_result = match &memory_scope {
                                            Some(scope) => crate::query_context::with_conversation_memory_scope(
                                                scope,
                                                retry_process,
                                            )
                                            .await,
                                            None => retry_process.await,
                                        };
                                        match retry_result {
                                            Ok(retry_output) => {
                                                tracing::info!(
                                                    "评估重试第{}次完成, 新回答长度={}",
                                                    attempt + 1,
                                                    retry_output.answer.len()
                                                );
                                                // 存入记忆脑
                                                {
                                                    let mem = this.memory_brain.lock().await;
                                                    let stored = match &memory_scope {
                                                        Some(scope) => mem.store_turns_scoped(
                                                            &retry_output.turns,
                                                            scope,
                                                        ),
                                                        None => {
                                                            mem.store_turns(&retry_output.turns)
                                                        }
                                                    };
                                                    if let Err(e) = stored {
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
                                        let _ = this.runtime_trace_tx.send(RuntimeExchange::new(
                                            &eval_exchange_id,
                                            "eval",
                                            "评估脑",
                                            "main",
                                            "主脑",
                                            ExchangeKind::Evaluation,
                                            ExchangePhase::Response,
                                            "评估执行失败",
                                            e.to_string(),
                                            ExchangeStatus::Failed,
                                            None,
                                        ));
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
                                    let _ = tx
                                        .send(ProgressEvent::TextDelta { text: notification })
                                        .await;
                                    tracing::info!(
                                        "异步子代理通知: {} status={:?}",
                                        result.agent_id,
                                        result.status
                                    );
                                }
                                brain_dispatch::MainLoopMessage::BrainTaskNotification {
                                    brain_id,
                                    result,
                                } => {
                                    let notification = format!("\n📡 副脑任务完成: {brain_id}\n",);
                                    let _ = tx
                                        .send(ProgressEvent::TextDelta { text: notification })
                                        .await;
                                    tracing::info!(
                                        "副脑任务通知: {brain_id} status={:?}",
                                        result.status
                                    );
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
                "=== 金字塔记忆统计 ===\n\
                 \x20 L1 全量基座: {}\n\
                 \x20 L2 任务摘要: {}\n\
                 \x20 L3 经验抽象: {}\n\
                 \x20 L4 潜意识: {}\n\
                 \x20 活跃人格: {}\n\
                 \x20 会话ID: {}\n",
                s.l1_count,
                s.l2_count,
                s.l3_count,
                if s.l4_exists { "有" } else { "无" },
                s.active_persona,
                s.session_id,
            ),
            Err(e) => format!("获取记忆统计失败: {e}"),
        }
    }

    /// 记忆统计（原始数据，兼容旧 MemoryStats 格式）
    pub async fn memory_stats_raw(&self) -> Result<MemoryStats, String> {
        let brain = self.memory_brain.lock().await;
        brain.stats_legacy().map_err(|e| format!("{e}"))
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

    // ─── 进化脑 v2 方法 ──────────────────────────────────────────

    /// 初始化 v2 进化脑调度器（在首次 /evo 调用时惰性初始化）。
    pub async fn ensure_evo_coordinator(&self) -> Result<(), String> {
        let mut guard = self.evo_coordinator.lock().await;
        if guard.is_none() {
            let base_dir =
                std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                    .join(".ai-brain")
                    .join("evolution");
            let config = EvoConfig::default();
            let coord = EvolutionCoordinator::new(&base_dir, config)
                .map_err(|e| format!("Failed to init evo coordinator: {e}"))?;
            *guard = Some(coord);
        }
        Ok(())
    }

    /// v2: 启动一次进化循环（后台 tokio::task）。
    ///
    /// `target_hint`: 可选的目标描述。如果为 None，由 coordinator 自动选择。
    pub async fn spawn_evolution(&self, target_hint: Option<String>) -> Result<(), String> {
        self.ensure_evo_coordinator().await?;

        // 防重入
        if *self.evo_running.borrow() {
            return Err("进化脑正在运行中，请稍后再试".into());
        }

        // 直接创建 LLM（不再从 EvolverBrain 获取）
        let llm: Arc<dyn brain_llm::LlmProvider> = try_create_llm_client("evolver")
            .or_else(|| try_create_llm_client("sensory"))
            .ok_or("无法创建 LLM 客户端")?;

        // 获取 skill 名称
        let skill_names: Vec<String> = self
            .skill_catalog
            .skills
            .iter()
            .map(|s| s.name.clone())
            .collect();

        // 获取金字塔根目录
        let pyramid_root = {
            let mem = self.memory_brain.lock().await;
            mem.storage().pyramid_root()
        };

        let shared = SharedResources {
            mcp_pool_info: String::new(),
            skill_names,
            memory: Arc::new(brain_evolver::PyramidMemoryAccess::new(pyramid_root)),
            web_search: Arc::new(brain_evolver::StubWebSearch),
        };

        // 如果指定了目标描述，创建临时 target
        if let Some(desc) = &target_hint {
            let mut guard = self.evo_coordinator.lock().await;
            if let Some(coord) = guard.as_mut() {
                use brain_evolver::target::{EvoTarget, TargetStatus};
                use chrono::Utc;
                let target = EvoTarget {
                    id: format!("tgt-{}", Utc::now().format("%Y%m%d%H%M%S")),
                    direction: desc.clone(),
                    description: desc.clone(),
                    priority: 1,
                    status: TargetStatus::Pending,
                    checkpoints: vec![],
                    created_at: Utc::now(),
                    related_skills: vec![],
                };
                coord.target_queue_mut().add_target(target).map_err(|e| e)?;
            }
        }

        // Pick target
        let target = {
            let guard = self.evo_coordinator.lock().await;
            if let Some(coord) = guard.as_ref() {
                coord.pick_next_target()
            } else {
                None
            }
        };

        let target = match target {
            Some(t) => t,
            None => return Err("没有可用的进化目标".into()),
        };

        // 构建系统 prompt（原 EvoOrchestrator.update_system_prompt 逻辑内联）
        let (system_prompt, log_id) = {
            let mut guard = self.evo_coordinator.lock().await;
            let coord = guard.as_mut().ok_or("coordinator 未初始化")?;
            let ctx = brain_evolver::EvoPromptContext {
                current_target: brain_evolver::describe_target(&target),
                existing_skill_names: coord
                    .capability_tree()
                    .domains
                    .iter()
                    .flat_map(|d| d.skills.clone())
                    .collect(),
                related_backlog_entries: coord
                    .backlog()
                    .query_sorted_by_priority()
                    .iter()
                    .map(|e| e.description.clone())
                    .take(5)
                    .collect(),
                token_budget: EvoConfig::default().token_budget_per_target,
                ..Default::default()
            };
            let prompt = brain_evolver::evo_prompt::build_evo_system_prompt(&ctx);
            let tid = target_id_from_candidate(&target);
            let log_id = coord.log_cycle_start(&tid);
            (prompt, log_id)
        };

        // 构建 CycleConfig
        let evo_config = EvoConfig::default();
        let cycle_config = CycleConfig {
            max_iterations: evo_config.max_iterations_per_target,
            token_budget_per_target: evo_config.token_budget_per_target,
            verify_threshold: evo_config.verify_threshold,
            system_prompt: system_prompt.clone(),
            ..CycleConfig::default()
        };

        // 创建 CycleRunner（替代 EvoOrchestrator）
        let mut cycle_runner = CycleRunner::with_resources(
            llm,
            cycle_config,
            shared.memory.clone(),
            shared.web_search.clone(),
        );
        cycle_runner.set_system_prompt(system_prompt);

        // Mark running
        let _ = self.evo_running.send(true);

        // Spawn background task
        let evo_running_tx = self.evo_running.clone();
        let evo_coordinator = self.evo_coordinator.clone();
        let handle = tokio::spawn(async move {
            tracing::info!("进化脑 v2 启动，开始进化循环...");

            let result = cycle_runner.run(&target).await;

            // 处理结果
            let mut guard = evo_coordinator.lock().await;
            if let Some(coord) = guard.as_mut() {
                match &result {
                    Ok(cycle_result) => {
                        let (phases, tokens, skills, resolved_backlog, status) =
                            extract_cycle_metadata(cycle_result);

                        let _ = coord.log_cycle_end(
                            &log_id,
                            phases,
                            tokens,
                            skills.clone(),
                            resolved_backlog,
                            status,
                        );

                        if cycle_result.is_success() {
                            let tid = target_id_from_candidate(&target);
                            let _ = coord.resolve_target(&tid, &log_id, skills);
                        }
                    }
                    Err(e) => {
                        let _ = coord.log_cycle_end(
                            &log_id,
                            vec![],
                            0,
                            vec![],
                            vec![],
                            brain_evolver::evo_log::EvoCycleStatus::Blocked,
                        );
                        tracing::error!("进化脑 v2 失败: {e}");
                    }
                }
            }

            tracing::info!("进化脑 v2 循环结束");
            let _ = evo_running_tx.send(false);
        });

        // Store handle
        let mut handle_guard = self.evo_handle.lock().await;
        *handle_guard = Some(handle);

        Ok(())
    }

    /// v2: 停止正在运行的进化循环。
    pub async fn stop_evolution(&self) -> Result<(), String> {
        let mut handle_guard = self.evo_handle.lock().await;
        if let Some(handle) = handle_guard.take() {
            handle.abort();
            let _ = self.evo_running.send(false);
            tracing::info!("进化脑 v2 已中止");
            Ok(())
        } else {
            Err("没有正在运行的进化任务".into())
        }
    }

    /// v2: 查询进化脑状态。
    pub async fn evo_status_v2(&self) -> String {
        let is_running = *self.evo_running.borrow();

        let coordinator_info = {
            let guard = self.evo_coordinator.lock().await;
            match guard.as_ref() {
                Some(coord) => {
                    let has_work = coord.has_pending_work();
                    let log_count = coord.log_store().latest().map_or(0, |_| 1);
                    format!("has_pending_work={has_work}, log_entries={log_count}",)
                }
                None => "未初始化".into(),
            }
        };

        format!("进化脑 v2 状态: running={is_running}, coordinator=[{coordinator_info}]")
    }

    /// v2: 获取进化调度器的可变引用（用于命令接口直接操作）。
    pub async fn evo_coordinator_mut(
        &self,
    ) -> tokio::sync::MutexGuard<'_, Option<EvolutionCoordinator>> {
        self.evo_coordinator.lock().await
    }

    /// 通知进化脑有用户活动（用于空闲检测）。
    pub async fn touch_evo_activity(&self) {
        let trigger = self.evo_trigger.lock().await;
        trigger.touch_activity();
    }

    /// 添加 backlog 条目（用于 EvalBrain/主脑诊断 → Backlog 集成）
    ///
    /// 将检测到的问题写入 EvolutionBacklog，作为进化脑的驱动力。
    pub async fn add_backlog_entry(
        &self,
        source: &str,
        category: &str,
        description: &str,
        severity: &str,
        context_snapshot: Option<&str>,
    ) -> Result<(), String> {
        // 确保 coordinator 已初始化
        self.ensure_evo_coordinator().await?;

        let mut guard = self.evo_coordinator.lock().await;
        if let Some(coord) = guard.as_mut() {
            use brain_evolver::backlog::{
                BacklogCategory, BacklogEntry, BacklogSource, BacklogStatus, Severity,
            };
            use chrono::Utc;

            // 映射 source
            let backlog_source = match source {
                "Eval" => BacklogSource::Eval,
                "SelfDiagnosis" => BacklogSource::SelfDiagnosis,
                "Memory" => BacklogSource::Memory,
                _ => BacklogSource::User,
            };

            // 映射 category
            let backlog_category = match category {
                "KnowledgeGap" => BacklogCategory::KnowledgeGap,
                "CodeQuality" => BacklogCategory::CodeQuality,
                "ReasoningWeakness" => BacklogCategory::ReasoningWeakness,
                "ToolMissing" => BacklogCategory::ToolMissing,
                _ => BacklogCategory::KnowledgeGap,
            };

            // 映射 severity
            let backlog_severity = match severity {
                "Critical" => Severity::Critical,
                "High" => Severity::High,
                "Medium" => Severity::Medium,
                _ => Severity::Low,
            };

            let entry = BacklogEntry {
                id: format!("blg_{}", Utc::now().format("%Y%m%d%H%M%S%f")),
                source: backlog_source,
                category: backlog_category,
                description: description.to_string(),
                severity: backlog_severity,
                frequency: 1,
                status: BacklogStatus::Pending,
                created_at: Utc::now(),
                context_snapshot: context_snapshot.map(|s| s.to_string()),
                resolved_at: None,
                evolution_log_id: None,
            };

            coord
                .backlog_mut()
                .add_entry(entry)
                .map_err(|e| format!("写入 backlog 失败: {}", e))?;

            tracing::info!(
                "Orchestrator 收到 backlog 条目: {} ({}/{})",
                description,
                source,
                severity
            );
            Ok(())
        } else {
            Err("EvolutionCoordinator 未初始化".into())
        }
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

        let _ = self.shutdown_tx.send(true);
        tracing::info!("AI Brain 正在关闭...");
    }

    /// 检查是否需要触发四步浓缩（每 N 轮），并在后台启动
    pub fn maybe_trigger_analysis(&self) {
        let (should, interval) = {
            let mem = match self.memory_brain.try_lock() {
                Ok(m) => m,
                Err(_) => return,
            };
            (
                mem.tick_and_should_analyze(),
                mem.persona_manager().analysis_interval(),
            )
        };

        if should {
            tracing::info!("达到 {} 轮，触发四步浓缩", interval);
            let (base_dir, session_id) = {
                let mem = match self.memory_brain.try_lock() {
                    Ok(m) => m,
                    Err(_) => return,
                };
                let dir = mem.base_dir().to_path_buf();
                let sid = mem.session_id().to_string();
                (dir, sid)
            };

            let llm = LlmConfig::load_default()
                .ok()
                .and_then(|c| Self::create_analyzer_llm_with_config(&c));
            let _ = tokio::spawn(async move {
                if let Some(llm) = llm {
                    let graph_db_path = Some(base_dir.join("graph").join("graph.db"));
                    let config = PyramidMemoryBrainConfig {
                        base_dir,
                        session_id,
                        graph_db_path,
                    };
                    if let Ok(brain) = PyramidMemoryBrain::new(config) {
                        let report = brain.concentrate(&llm).await;
                        tracing::info!(
                            "四步浓缩完成: tasks={}, types={}, triggers={}, narrative={}字, profile_updated={}, errors={}",
                            report.step1_tasks, report.step2_types, report.step3_triggers,
                            report.step3_narrative_chars, report.step4_profile_updated, report.errors.len(),
                        );
                    }
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

    /// 强制执行一次四步浓缩（保留供手动调用）
    #[allow(dead_code)]
    async fn run_analysis_force(&self) {
        let (base_dir, session_id) = {
            let mem = match self.memory_brain.try_lock() {
                Ok(m) => m,
                Err(_) => return,
            };
            let dir = mem.base_dir().to_path_buf();
            let sid = mem.session_id().to_string();
            (dir, sid)
        };

        let llm = LlmConfig::load_default()
            .ok()
            .and_then(|c| Self::create_analyzer_llm_with_config(&c));
        if let Some(llm) = llm {
            tracing::info!("正在执行四步浓缩（关闭时强制触发）...");
            let graph_db_path = Some(base_dir.join("graph").join("graph.db"));
            let config = PyramidMemoryBrainConfig {
                base_dir,
                session_id,
                graph_db_path,
            };
            if let Ok(brain) = PyramidMemoryBrain::new(config) {
                let report = brain.concentrate(&llm).await;
                tracing::info!(
                    "四步浓缩完成: tasks={}, types={}, triggers={}, narrative={}字, profile_updated={}, errors={}",
                    report.step1_tasks, report.step2_types, report.step3_triggers,
                    report.step3_narrative_chars, report.step4_profile_updated, report.errors.len(),
                );
            }
        } else {
            tracing::warn!("无法创建记忆脑 LLM 客户端，跳过关闭时的四步浓缩");
        }
    }

    /// 创建四步分析用的 LLM 客户端（统一方法，接受外部配置避免重复读盘）
    fn create_analyzer_llm_with_config(config: &LlmConfig) -> Option<AnalyzerLlm> {
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
    memory_brain: Arc<Mutex<PyramidMemoryBrain>>,
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
    memory_brain: &Arc<Mutex<PyramidMemoryBrain>>,
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

struct NovelWriterClient {
    provider: Arc<dyn brain_llm::LlmProvider>,
    model: novel_workflow::ProfileModel,
    max_output_tokens: u32,
    temperature: f64,
}

const NOVEL_WRITER_ROUTE_ERROR_CHARS: usize = 128;
const NOVEL_WRITER_DETAIL_ERROR_CHARS: usize = 2_048;

/// 创建 Novel Writer 的显式路由；配置错误直接阻止启动，不进入不可用占位状态。
fn create_novel_writer_client(config: &LlmConfig) -> Result<NovelWriterClient, String> {
    let provider_name = config.provider_for_brain("main");
    let model_name = config.model_for_brain("main");
    let route_provider = crate::novel_adapters::bounded_sensitive_text(
        provider_name,
        NOVEL_WRITER_ROUTE_ERROR_CHARS,
    );
    let route_model =
        crate::novel_adapters::bounded_sensitive_text(model_name, NOVEL_WRITER_ROUTE_ERROR_CHARS);
    let route = format!("provider={route_provider}, model={route_model}");
    let provider = config
        .create_brain_client("main")
        .map_err(|error| match error {
            brain_llm::LlmError::ProviderNotFound(name) => {
                let name = crate::novel_adapters::bounded_sensitive_text(
                    &name,
                    NOVEL_WRITER_ROUTE_ERROR_CHARS,
                );
                format!("初始化 Novel Writer LLM 失败 ({route}): Provider 不存在: {name}")
            }
            brain_llm::LlmError::ApiKeyNotFound(name) => {
                let name = crate::novel_adapters::bounded_sensitive_text(
                    &name,
                    NOVEL_WRITER_ROUTE_ERROR_CHARS,
                );
                format!("初始化 Novel Writer LLM 失败 ({route}): API Key 未配置: {name}")
            }
            other => {
                let detail = crate::novel_adapters::bounded_sensitive_text(
                    &other.to_string(),
                    NOVEL_WRITER_DETAIL_ERROR_CHARS,
                );
                format!("初始化 Novel Writer LLM 失败 ({route}): {detail}")
            }
        })?;
    let (max_output_tokens, temperature) = config.params_for_brain("main");
    Ok(NovelWriterClient {
        provider: Arc::from(provider),
        model: novel_workflow::ProfileModel::new(provider_name, model_name),
        max_output_tokens,
        temperature,
    })
}

/// 创建感知脑 LLM（不降级，失败直接报错）
fn create_sensory_llm(config: &LlmConfig) -> Result<LlmResult, String> {
    let client = config
        .create_brain_client("sensory")
        .map_err(|e| format!("LLM 客户端创建失败: {e}\n请检查 ZHIPU_API_KEY 是否已设置"))?;
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
    llm_config: &LlmConfig,
    memory_brain: Option<Arc<Mutex<PyramidMemoryBrain>>>,
    dispatch: brain_dispatch::TokioDispatch,
    runtime_trace_tx: broadcast::Sender<RuntimeExchange>,
    tool_execution_context: ToolExecutionContext,
    // novel_application: Arc<dyn TaskApplicationPort>,
) -> (
    Arc<Mutex<Option<MainBrain>>>,
    Option<PluginManager>,
    Arc<SkillCatalog>,
    Arc<McpClientPool>,
) {
    let client = match llm_config.create_brain_client("main") {
        Ok(c) => Arc::from(c) as Arc<dyn brain_llm::LlmProvider>,
        Err(error) => {
            tracing::warn!("v2 MainBrain: 主脑 LLM 不可用，跳过创建（回声模式）: {error}");
            return (
                Arc::new(Mutex::new(None)),
                None,
                Arc::new(SkillCatalog {
                    skills: vec![],
                    packs: vec![],
                    bootstrap_skills: vec![],
                }),
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
    match install_builtin_skills(&ai_brain_dir) {
        Ok(_root) => {
            // Novel 工作流已停用：技能文件继续保留，但不再加入主脑 SkillCatalog。
            // skill_roots.push(_root);
        }
        Err(error) => tracing::warn!("准备内置技能失败: {error}"),
    }

    let skill_catalog = SkillCatalog::scan_all(&skill_roots).unwrap_or_else(|e| {
        tracing::warn!("扫描技能失败: {e}");
        SkillCatalog {
            skills: vec![],
            packs: vec![],
            bootstrap_skills: vec![],
        }
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
            .with_mcp_pool(mcp_pool.clone())
            .with_runtime_trace_sender(runtime_trace_tx),
        // Novel 工作流已停用：保留执行器实现，但不再注入应用端口。
        // .with_novel_application(Some(novel_application)),
    );

    let brain_config = BrainConfig::default();
    let (main_mt, main_temp) = llm_config.params_for_brain("main");
    tracing::info!(
        "v2 MainBrain: provider={}, model={}",
        llm_config.provider_for_brain("main"),
        llm_config.model_for_brain("main")
    );
    let mut brain = MainBrain::new_in_context(
        client,
        tool_executor,
        brain_config,
        main_mt,
        main_temp,
        tool_execution_context,
    );

    // 注册所有 MVP 工具（bash、read_file、write_file、edit_file、glob、grep）
    let tool_defs = crate::real_tool_executor::mvp_tool_definitions();
    tracing::info!("v2 MainBrain: 注册 {} 个工具", tool_defs.len());
    brain.register_tools(tool_defs);

    // 注入技能摘要到 system prompt，让 LLM 感知可用技能
    let skill_summary = skill_catalog.summary_for_prompt();
    if !skill_summary.is_empty() {
        brain.inject_skill_summary(skill_summary);
    }

    (
        Arc::new(Mutex::new(Some(brain))),
        plugin_mgr,
        skill_catalog,
        mcp_pool,
    )
}

const NOVEL_KNOWLEDGE_SOURCE_TYPES: [&str; 4] = [
    "novel.canon",
    "novel.artifact",
    "novel.memory",
    "novel.project_resource",
];

fn create_knowledge_registries(
) -> Result<(Arc<KnowledgeSchemaRegistry>, Arc<ProjectionAdapterRegistry>), String> {
    let schemas = Arc::new(KnowledgeSchemaRegistry::new());
    schemas
        .register(
            KnowledgeSchemaBundle::new("platform.core", NamespaceId::from("platform.core"), 1)
                .with_memory_type(MemoryTypeSchema::new(
                    MemoryTypeId::from("platform.note"),
                    [
                        ScopeTypeId::from("room"),
                        ScopeTypeId::from("member"),
                        ScopeTypeId::from("instance_run"),
                    ],
                ))
                .with_memory_type(MemoryTypeSchema::new(
                    MemoryTypeId::from("platform.summary"),
                    [ScopeTypeId::from("room"), ScopeTypeId::from("member")],
                )),
        )
        .map_err(|error| format!("注册平台知识 Schema 失败: {error}"))?;
    schemas
        .register(novel_knowledge_adapter::novel_schema_bundle())
        .map_err(|error| format!("注册 Novel 知识 Schema 失败: {error}"))?;

    let projection_adapters = Arc::new(ProjectionAdapterRegistry::new());
    for source_type in NOVEL_KNOWLEDGE_SOURCE_TYPES {
        projection_adapters
            .register(Arc::new(
                novel_knowledge_adapter::NovelKnowledgeAdapter::for_source_type(
                    ResourceTypeId::from(source_type),
                ),
            ))
            .map_err(|error| format!("注册 Novel 知识投影 Adapter 失败: {error}"))?;
    }
    Ok((schemas, projection_adapters))
}

fn create_knowledge_runtime(
    base_dir: &Path,
) -> Result<
    (
        Arc<ContextBuilder>,
        Arc<ProjectionAdapterRegistry>,
        Arc<GenericMemoryStore>,
        Option<Arc<GenericGraphStore>>,
    ),
    String,
> {
    let (schemas, projection_adapters) = create_knowledge_registries()?;

    let memory_store = Arc::new(
        GenericMemoryStore::open(base_dir.join("memory.db"), Arc::clone(&schemas))
            .map_err(|error| format!("初始化通用 MemoryStore 失败: {error}"))?,
    );
    let memory: Arc<dyn MemoryQueryPort> = memory_store.clone();
    let (graph, graph_store): (Arc<dyn GraphQueryPort>, Option<Arc<GenericGraphStore>>) =
        match GenericGraphStore::open(base_dir.join("graph.db"), Arc::clone(&schemas)) {
            Ok(store) => {
                let store = Arc::new(store);
                (store.clone(), Some(store))
            }
            Err(error) => {
                tracing::warn!("初始化通用 GraphStore 失败，本次上下文将降级: {error}");
                (
                    Arc::new(UnavailableGraphQuery {
                        reason: error.to_string(),
                    }),
                    None,
                )
            }
        };
    let resolvers = Arc::new(ContentResolverRegistry::new());
    Ok((
        Arc::new(ContextBuilder::new(memory, graph, resolvers)),
        projection_adapters,
        memory_store,
        graph_store,
    ))
}

/// 创建所有副脑
fn create_sub_brains() -> Result<
    (
        PyramidMemoryBrain,
        ReasoningBrain,
        MotorBrain,
        ValidationBrain,
        EvaluationBrain,
    ),
    String,
> {
    let memory = PyramidMemoryBrain::new(PyramidMemoryBrainConfig::default())
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
    use brain_llm::config::{InstanceModelConfig, ProviderConfig, ProviderKind};
    use knowledge_core::{ContextBlock, ContextBlockInput};

    #[test]
    fn general_eval_brain_is_opt_in() {
        let default_config = HooksConfig::default();
        assert!(!general_eval_enabled(&default_config));

        let mut enabled = HooksConfig::default();
        enabled.eval_gate.enabled = true;
        assert!(general_eval_enabled(&enabled));
        enabled.enabled = false;
        assert!(!general_eval_enabled(&enabled));
    }

    #[test]
    fn knowledge_runtime_registers_novel_schema_and_all_source_adapters() {
        let (schemas, adapters) = create_knowledge_registries().unwrap();
        assert!(schemas.bundles().iter().any(|bundle| {
            bundle.schema_id == novel_knowledge_adapter::NOVEL_SCHEMA_ID
                && bundle.namespace.as_str() == novel_knowledge_adapter::NOVEL_NAMESPACE
        }));
        assert!(schemas
            .memory_type(&MemoryTypeId::from("novel.chapter"))
            .is_some());
        assert!(schemas
            .node_type(&knowledge_core::NodeTypeId::from("novel.character"))
            .is_some());
        assert!(schemas
            .relation_type(&knowledge_core::RelationTypeId::from("novel.related_to"))
            .is_some());

        let namespace = NamespaceId::from(novel_knowledge_adapter::NOVEL_NAMESPACE);
        for source_type in NOVEL_KNOWLEDGE_SOURCE_TYPES {
            let source_type = ResourceTypeId::from(source_type);
            let adapter = adapters
                .get(&namespace, &source_type)
                .unwrap_or_else(|| panic!("missing Novel adapter for {source_type}"));
            assert_eq!(adapter.namespace(), namespace);
            assert_eq!(adapter.source_type(), source_type);
            assert_eq!(
                adapter.version(),
                novel_knowledge_adapter::NOVEL_SCHEMA_VERSION
            );
        }
    }

    #[test]
    fn member_model_inputs_are_derived_only_from_the_frozen_snapshot() {
        let snapshot = KnowledgeContextSnapshot::new(
            "context-member-run",
            vec![
                ContextBlock::from_input(ContextBlockInput::new(
                    "policy",
                    ContextBlockKind::SystemPolicy,
                    "member policy",
                ))
                .unwrap(),
                ContextBlock::from_input(ContextBlockInput::new(
                    "history-user",
                    ContextBlockKind::ConversationUser,
                    "earlier question",
                ))
                .unwrap(),
                ContextBlock::from_input(ContextBlockInput::new(
                    "history-assistant",
                    ContextBlockKind::ConversationAssistant,
                    "earlier answer",
                ))
                .unwrap(),
                ContextBlock::from_input(ContextBlockInput::new(
                    "memory",
                    ContextBlockKind::Memory,
                    "authorized memory",
                ))
                .unwrap(),
                ContextBlock::from_input(ContextBlockInput::new(
                    "reply-reference:event-1",
                    ContextBlockKind::ConversationReference,
                    "[被回复引用]\n较早的权威消息",
                ))
                .unwrap(),
                ContextBlock::from_input(ContextBlockInput::new(
                    "current",
                    ContextBlockKind::CurrentInput,
                    "current question",
                ))
                .unwrap(),
            ],
        )
        .unwrap();

        let (input, history, system) = member_inputs_from_snapshot(&snapshot).unwrap();
        assert_eq!(input, "current question");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "earlier question");
        assert_eq!(history[1].role, "assistant");
        assert!(system.contains("member policy"));
        assert!(system.contains("authorized memory"));
        assert!(system.contains("[被回复引用]"));
        assert!(system.contains("较早的权威消息"));

        let mut tampered = snapshot;
        tampered.blocks[0].content = "changed after freeze".into();
        assert!(member_inputs_from_snapshot(&tampered).is_err());
    }

    #[test]
    fn member_reasoning_depth_applies_bounded_output_policy() {
        assert_eq!(
            resolve_member_reasoning_tokens(32_768, "low").unwrap(),
            4_096
        );
        assert_eq!(
            resolve_member_reasoning_tokens(32_768, "medium").unwrap(),
            8_192
        );
        assert_eq!(
            resolve_member_reasoning_tokens(32_768, "high").unwrap(),
            32_768
        );
        assert_eq!(
            resolve_member_reasoning_tokens(2_048, "low").unwrap(),
            2_048
        );
        assert!(resolve_member_reasoning_tokens(32_768, "max").is_err());
    }

    #[test]
    fn member_execution_uses_injected_catalog_config_and_rejects_unknown_policy() {
        let mut config = LlmConfig::default_config();
        config.llm.providers.insert(
            "relay".into(),
            ProviderConfig {
                api_base: "https://relay.example.com/v1".into(),
                api_key: Some("test-key".into()),
                ..ProviderConfig::default()
            },
        );
        config.llm.instance_models = vec![InstanceModelConfig {
            id: "gemini-2-5-flash".into(),
            label: "Gemini 2.5 Flash".into(),
            provider: "relay".into(),
            model: "gemini-2.5-flash".into(),
        }];

        let (client, policy) = create_member_execution_client(&config, "gemini-2-5-flash").unwrap();
        assert_eq!(policy.label, "Gemini 2.5 Flash");
        assert_eq!(client.model(), "gemini-2.5-flash");

        let error = match create_member_execution_client(&config, "not-configured") {
            Ok(_) => panic!("未知成员模型策略不应创建客户端"),
            Err(error) => error,
        };
        assert!(!error.execution_started());
        assert!(error
            .to_string()
            .contains("成员模型策略未配置: not-configured"));
    }

    #[test]
    fn novel_writer_initialization_reports_explicit_route_errors() {
        let mut unknown = LlmConfig::default_config();
        unknown
            .llm
            .brain_providers
            .insert("main".into(), "missing-provider".into());
        unknown
            .llm
            .brain_models
            .insert("main".into(), "missing-model".into());
        let error = match create_novel_writer_client(&unknown) {
            Ok(_) => panic!("未知 provider 不应创建 Novel Writer"),
            Err(error) => error,
        };
        for required in ["provider=missing-provider", "model=missing-model", "不存在"] {
            assert!(
                error.contains(required),
                "error missing {required}: {error}"
            );
        }

        let mut missing_key = LlmConfig::default_config();
        missing_key.llm.providers.insert(
            "writer-without-key".into(),
            ProviderConfig {
                api_base: "https://writer.invalid/v1".into(),
                api_key_env: "AI_BRAIN_TEST_KEY_THAT_MUST_NOT_EXIST_7F8435".into(),
                api_key: None,
                ..ProviderConfig::default()
            },
        );
        missing_key
            .llm
            .brain_providers
            .insert("main".into(), "writer-without-key".into());
        missing_key
            .llm
            .brain_models
            .insert("main".into(), "writer-model".into());
        let error = match create_novel_writer_client(&missing_key) {
            Ok(_) => panic!("缺 API key 不应创建 Novel Writer"),
            Err(error) => error,
        };
        for required in [
            "provider=writer-without-key",
            "model=writer-model",
            "API Key",
        ] {
            assert!(
                error.contains(required),
                "error missing {required}: {error}"
            );
        }

        let route_secret = "NOVEL_WRITER_ROUTE_SECRET";
        let mut malicious_route = LlmConfig::default_config();
        malicious_route.llm.brain_providers.insert(
            "main".into(),
            format!("missing\nAuthorization: Bearer {route_secret}"),
        );
        malicious_route.llm.brain_models.insert(
            "main".into(),
            format!("model\r\napi_key=\"{route_secret}\""),
        );
        let error = match create_novel_writer_client(&malicious_route) {
            Ok(_) => panic!("恶意 route 不应创建 Novel Writer"),
            Err(error) => error,
        };
        assert!(!error.contains(route_secret), "启动错误泄漏 route: {error}");
        assert!(!error.contains('\n'), "启动错误允许换行注入: {error}");
        assert!(!error.contains('\r'), "启动错误允许回车注入: {error}");

        let proxy_secret = "NOVEL_WRITER_PROXY_SECRET";
        let mut invalid_proxy = LlmConfig::default_config();
        invalid_proxy.llm.providers.insert(
            "writer-invalid-proxy".into(),
            ProviderConfig {
                api_base: "https://writer.invalid/v1".into(),
                api_key: Some("test-key".into()),
                kind: ProviderKind::Gemini,
                proxy: Some(format!("not-a-url\nAuthorization: Bearer {proxy_secret}")),
                ..ProviderConfig::default()
            },
        );
        invalid_proxy
            .llm
            .brain_providers
            .insert("main".into(), "writer-invalid-proxy".into());
        invalid_proxy
            .llm
            .brain_models
            .insert("main".into(), "writer-model".into());
        let error = match create_novel_writer_client(&invalid_proxy) {
            Ok(_) => panic!("无效 proxy 不应创建 Novel Writer"),
            Err(error) => error,
        };
        assert!(!error.contains(proxy_secret), "启动错误泄漏 proxy: {error}");
        assert!(!error.contains('\n'), "启动错误允许换行注入: {error}");
        assert!(error.chars().count() < 2_500, "启动错误未限长");
    }

    #[test]
    fn builtin_novel_writing_skill_is_seeded_and_loadable() {
        let dir = tempfile::tempdir().unwrap();
        let root = install_builtin_skills(dir.path()).unwrap();
        let catalog = SkillCatalog::scan_all(&[root.clone()]).unwrap();
        let skill = catalog
            .resolve(NOVEL_WRITING_SKILL_NAME)
            .expect("built-in Novel writing skill");
        assert!(!skill.is_bootstrap);
        assert!(skill.description.contains("小说创作"));
        let body = catalog.load_content(skill).unwrap();
        assert!(body.contains("## 主脑职责"));
        assert!(body.contains("novel_project"));
        assert!(body.contains("novel_task"));
        let summary = catalog.summary_for_prompt();
        assert!(summary.contains("<name>novel-writing-workflow</name>"));
        assert!(summary.contains("在创建、修改、审校或发布作品前使用"));

        let skill_path = root.join(NOVEL_WRITING_SKILL_NAME).join("SKILL.md");
        std::fs::write(&skill_path, "stale").unwrap();
        install_builtin_skills(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(skill_path).unwrap(),
            NOVEL_WRITING_SKILL
        );
    }

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
        assert_eq!(
            orch.task_count(),
            4,
            "应该有 4 个任务（3副脑+1dispatch_loop）"
        );
        orch.shutdown();
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
