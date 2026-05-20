use serde::{Deserialize, Serialize};

/// 副脑唯一标识
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct BrainId(pub String);

#[allow(clippy::must_use_candidate)]
impl BrainId {
    pub fn sensory() -> Self {
        Self("sensory".into())
    }
    pub fn master() -> Self {
        Self("master".into())
    }
    pub fn reasoning() -> Self {
        Self("reasoning".into())
    }
    pub fn memory() -> Self {
        Self("memory".into())
    }
    pub fn motor() -> Self {
        Self("motor".into())
    }
    pub fn validation() -> Self {
        Self("validation".into())
    }
    pub fn evaluation() -> Self {
        Self("evaluation".into())
    }
    pub fn evolver() -> Self {
        Self("evolver".into())
    }
}

impl std::fmt::Display for BrainId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 副脑种类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrainKind {
    Sensory,
    Master,
    Reasoning,
    Memory,
    Motor,
    Validation,
    Evaluation,
    Evolver,
}

/// 权重 [0.1, 1.0]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Weight(pub f64);

#[allow(clippy::must_use_candidate)]
impl Weight {
    pub fn default_value() -> Self {
        Self(0.5)
    }

    /// 强化权重
    pub fn strengthen(&mut self, delta: f64) {
        self.0 = (self.0 + delta).clamp(0.1, 1.0);
    }

    /// 弱化权重
    pub fn weaken(&mut self, delta: f64) {
        self.0 = (self.0 - delta).clamp(0.1, 1.0);
    }

    pub fn value(&self) -> f64 {
        self.0
    }
}

impl Default for Weight {
    fn default() -> Self {
        Self::default_value()
    }
}

/// 消息优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessagePriority {
    High,
    Normal,
    Low,
}

/// 环境上下文（感知脑注入）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainContext {
    pub current_date: String,
    pub cwd: String,
    pub git_branch: Option<String>,
    pub platform: String,
}

/// 知识来源
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KnowledgeSource {
    WebSearch {
        url: String,
    },
    Memory {
        memory_id: String,
        layer: MemoryLayer,
    },
    LlmReasoning {
        model: String,
    },
    UserConfirmation,
    OtherBrain {
        brain_id: BrainId,
    },
}

/// 记忆层级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryLayer {
    TaskSummary, // L0
    EventIndex,  // L1
    ShortTerm,   // L2
    Raw,         // L3
}

/// 快思考结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastThinkResult {
    pub relevant: bool,
    pub confidence: f64,
    pub summary: Option<String>,
    pub suggested_tools: Vec<String>,
    pub matched_experience: Option<String>,
}

/// 慢思考结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlowThinkResult {
    pub conclusion: String,
    pub reasoning_path: Vec<String>,
    pub confidence: f64,
    pub sources: Vec<KnowledgeSource>,
    pub new_experience: Option<NewExperience>,
}

/// 新经验（慢思考产出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewExperience {
    pub trigger_pattern: String,
    pub reasoning_path: Vec<String>,
    pub tools_used: Vec<String>,
}

/// 思考上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkContext {
    pub related_memories: Vec<MemoryEntry>,
    pub task_history: Vec<String>,
}

/// 记忆条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub tags: Vec<String>,
    pub layer: MemoryLayer,
    pub importance: f64,
    pub source: KnowledgeSource,
    pub confidence: f64,
    pub reference_count: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_accessed: chrono::DateTime<chrono::Utc>,
    pub consolidated: bool,
}

/// 协作消息类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollaborationKind {
    Request,
    Response,
    Dispatch,
}

/// 安全校验结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyCheckResult {
    pub safe: bool,
    pub risk_level: RiskLevel,
    pub reason: Option<String>,
    pub requires_user_approval: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// 真实性校验结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruthfulnessResult {
    pub confidence: f64,
    pub source_analysis: Vec<SourceAnalysis>,
    pub cross_verified: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAnalysis {
    pub source: KnowledgeSource,
    pub reliability: f64,
    pub supports_claim: bool,
}

/// 工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    pub input: serde_json::Value,
    pub validated: bool,
    pub validation_id: Option<String>,
}

/// 工具执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    pub tool_name: String,
    pub output: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

/// 工具描述
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// 上下文快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub brain_id: BrainId,
    pub message_count: usize,
    pub health_score: f64,
}

/// 评估结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    pub overall_health: f64,
    pub brain_reports: Vec<BrainHealthReport>,
    pub slim_instructions: Vec<SlimInstruction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainHealthReport {
    pub brain_id: BrainId,
    pub health_score: f64,
    pub usage_percent: f64,
    pub redundancy_score: f64,
    pub stale_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SlimInstruction {
    Delete {
        message_ids: Vec<String>,
        reason: String,
    },
    Compress {
        message_ids: Vec<String>,
        summary: String,
    },
    Preserve {
        message_ids: Vec<String>,
        reason: String,
    },
}

/// 召回查询
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallQuery {
    pub keywords: Vec<String>,
    pub tags: Vec<String>,
    pub max_results: usize,
    pub min_importance: f64,
    pub layers: Vec<MemoryLayer>,
}

impl Default for RecallQuery {
    fn default() -> Self {
        Self {
            keywords: Vec::new(),
            tags: Vec::new(),
            max_results: 10,
            min_importance: 0.2,
            layers: vec![
                MemoryLayer::TaskSummary,
                MemoryLayer::EventIndex,
                MemoryLayer::ShortTerm,
            ],
        }
    }
}

/// 巩固报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationReport {
    pub task_summaries_created: u32,
    pub event_indexes_created: u32,
    pub memories_consolidated: u32,
    pub duration_ms: u64,
}

/// 记忆统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub l0_count: u32,
    pub l1_count: u32,
    pub l2_count: u32,
    pub l3_count: u32,
    pub total_size_bytes: u64,
}

/// 主脑输出
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterOutput {
    pub answer: String,
    pub confidence: f64,
    pub sources: Vec<KnowledgeSource>,
    pub participating_brains: Vec<BrainId>,
    pub usage: TurnUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnUsage {
    pub total_tokens: u64,
    pub llm_calls: u32,
    pub duration_ms: u64,
    /// 累计 prompt tokens（LLM 返回的真实值）
    #[serde(default)]
    pub prompt_tokens: u64,
    /// 累计 completion tokens
    #[serde(default)]
    pub completion_tokens: u64,
}

/// 任务上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    pub input: BroadcastMessage,
    pub start_at: chrono::DateTime<chrono::Utc>,
    pub phase: TaskPhase,
    pub brain_responses: std::collections::HashMap<BrainId, BrainResponse>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskPhase {
    WaitingFastThink,
    DispatchingSlowThink,
    WaitingSlowThink,
    Validating,
    Finalizing,
}

// === 消息类型（brain-bus 依赖的核心消息结构） ===

/// 广播消息（通道1）— 感知脑 → 所有副脑
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastMessage {
    pub content: String,
    pub raw_input: String,
    pub context: BrainContext,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// 协作消息（通道2）— 副脑间点对点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationMessage {
    pub id: String,
    pub from: BrainId,
    pub to: Vec<BrainId>,
    pub correlation_id: Option<String>,
    pub hop_count: u32,
    pub priority: MessagePriority,
    pub content: String,
    pub kind: CollaborationKind,
}

/// 副脑响应（通道3）— 副脑 → 主脑
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainResponse {
    pub from: BrainId,
    pub relevance: f64,
    pub confidence: f64,
    pub result: BrainResponsePayload,
    pub need_slow_think: bool,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// 响应载荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrainResponsePayload {
    FastThink(FastThinkResult),
    SlowThink(SlowThinkResult),
    SafetyCheck(SafetyCheckResult),
    TruthfulnessCheck(TruthfulnessResult),
    MemoryRecall(Vec<MemoryEntry>),
    ToolResult(ToolExecutionResult),
    Evaluation(EvaluationResult),
    NotRelevant {
        reason: String,
    },
    /// 副脑已收到调度，正在处理中（ack 信号）
    Processing,
}

// ─── 对话消息（主脑历史管理） ─────────────────────────────────────────

/// 消息角色（内部对话历史用，与 brain_llm::MessageRole 分离）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
    Evaluator,
}

/// 对话消息（主脑内部历史记录）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl ConversationMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            timestamp: chrono::Utc::now(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            timestamp: chrono::Utc::now(),
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: content.into(),
            timestamp: chrono::Utc::now(),
        }
    }

    pub fn evaluator(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Evaluator,
            content: content.into(),
            timestamp: chrono::Utc::now(),
        }
    }
}

// ─── 主脑输出 ──────────────────────────────────────────────────────

/// 工具调用记录（L3 存储用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// 工具名
    pub tool_name: String,
    /// 工具输入（JSON）
    pub input: serde_json::Value,
    /// 工具输出
    pub output: String,
    /// 执行耗时 ms
    pub duration_ms: u64,
    /// 是否出错
    pub is_error: bool,
}

/// 一轮对话的完整轨迹（L3 存储用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    /// 角色
    pub role: TurnRole,
    /// 文本内容
    pub content: String,
    /// 工具调用（仅 role=ToolCall 时有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCallRecord>,
    /// 时间戳
    pub timestamp: String,
}

/// 轨迹角色
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TurnRole {
    User,
    Assistant,
    ToolCall,
    ToolResult,
}

/// 主脑输出（直接给用户的结果）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MainBrainOutput {
    pub answer: String,
    pub usage: TurnUsage,
    /// 完整对话轨迹（用户→助手→工具调用→工具结果→...→最终回答）
    #[serde(default)]
    pub turns: Vec<TurnRecord>,
}

// ─── 进度事件 ──────────────────────────────────────────────────────

/// oneshot::Sender 的 Debug 包装（oneshot::Sender 不 impl Debug）
pub struct UserResponseSender(pub tokio::sync::oneshot::Sender<String>);

impl std::fmt::Debug for UserResponseSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UserResponseSender")
    }
}

/// 进度事件（主脑→TUI/终端 的实时通知）
#[derive(Debug)]
pub enum ProgressEvent {
    Connecting {
        brain: String,
        model: String,
    },
    Thinking {
        brain: String,
    },
    TextDelta {
        text: String,
    },
    /// 思考内容增量（与 TextDelta 分离，TUI 默认不显示，Ctrl+E 可切换）
    ThinkingDelta {
        content: String,
    },
    ToolStart {
        brain: String,
        tool_name: String,
        input: String,
    },
    ToolDone {
        brain: String,
        tool_name: String,
        duration_ms: u64,
        output_preview: String,
        is_error: bool,
    },
    MemoryInjected {
        count: usize,
        preview: String,
    },
    /// 记忆详情（verbose 模式可见）
    MemoryDetail {
        memories: Vec<String>,
    },
    EvaluationStart,
    EvaluationResult {
        passed: bool,
        feedback: String,
    },
    /// 评估详情（verbose 模式可见）
    EvaluationDetail {
        score: f64,
        reports: Vec<BrainHealthReport>,
        instructions: Vec<SlimInstruction>,
    },
    Evaluating,
    LlmRetry {
        attempt: u32,
        max_attempts: u32,
        error: String,
    },
    /// 主脑询问用户问题，等待用户响应
    AskUser {
        question: String,
        options: Option<Vec<String>>,
        response_tx: UserResponseSender,
    },
    Done,
}

// ─── 用户画像 ──────────────────────────────────────────────────────

/// 用户画像（四步分析第二步产出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub explicit_preferences: Vec<String>,
    pub implicit_preferences: Vec<String>,
    pub taboos: Vec<String>,
    pub habits: Vec<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl Default for UserProfile {
    fn default() -> Self {
        Self {
            explicit_preferences: Vec::new(),
            implicit_preferences: Vec::new(),
            taboos: Vec::new(),
            habits: Vec::new(),
            updated_at: chrono::Utc::now(),
        }
    }
}

// ─── 踩坑记录 ──────────────────────────────────────────────────────

/// 踩坑类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PitfallCategory {
    ToolFailure,
    WrongAnswer,
    LazyBehavior,
    FormatIssue,
    Other,
}

/// 踩坑记录（四步分析第三步产出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PitfallRecord {
    pub id: String,
    pub category: PitfallCategory,
    pub description: String,
    pub user_correction: Option<String>,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub occurrence_count: u32,
    /// 被后续记忆迭代取代（不再召回，保留审计）
    #[serde(default)]
    pub superseded: bool,
}

// ─── 自进化规则 ────────────────────────────────────────────────────

/// 自进化规则（四步分析第四步产出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionRule {
    pub id: String,
    pub rule: String,
    pub source_pitfall_ids: Vec<String>,
    pub priority: u8,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 被后续记忆迭代取代（不再召回，保留审计）
    #[serde(default)]
    pub superseded: bool,
}

// ─── 用户评估要求 ──────────────────────────────────────────────────

/// 用户评估要求（动态积累，从用户反馈中提炼）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRequirement {
    /// 唯一标识
    pub id: String,
    /// 要求内容
    pub content: String,
    /// 来源（"用户反馈" / "记忆脑分析" / "系统默认"）
    pub source: String,
    /// 创建时间
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 是否已废弃
    #[serde(default)]
    pub superseded: bool,
}

// ─── 脑状态快照 ────────────────────────────────────────────────────

/// 来源引用类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceRefKind {
    Message,
}

/// 来源引用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    pub kind: SourceRefKind,
    pub reference: String,
    pub storage_id: String,
}

/// 脑状态快照（记忆脑→主脑 上下文重建用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainState {
    pub fact_summary: String,
    pub user_profile: UserProfile,
    pub active_pitfalls: Vec<PitfallRecord>,
    pub evolution_rules: Vec<EvolutionRule>,
    pub index_entries: Vec<SourceRef>,
    pub snapshot_at: chrono::DateTime<chrono::Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brain_id_display() {
        assert_eq!(BrainId::sensory().to_string(), "sensory");
        assert_eq!(BrainId::master().to_string(), "master");
    }

    #[test]
    fn weight_clamp() {
        let mut w = Weight(0.5);
        w.strengthen(1.0);
        assert!((w.value() - 1.0).abs() < f64::EPSILON);

        w.weaken(2.0);
        assert!((w.value() - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn recall_query_default_layers() {
        let q = RecallQuery::default();
        assert!(q.layers.contains(&MemoryLayer::TaskSummary));
        assert!(q.layers.contains(&MemoryLayer::EventIndex));
        assert!(q.layers.contains(&MemoryLayer::ShortTerm));
        assert!(!q.layers.contains(&MemoryLayer::Raw)); // 默认不搜原始层
    }

    #[test]
    fn serialize_deserialize_broadcast_message() {
        let msg = BroadcastMessage {
            content: "用户查询节假日".into(),
            raw_input: "这个月有节假日吗？".into(),
            context: BrainContext {
                current_date: "2026-04-03".into(),
                cwd: "/home".into(),
                git_branch: Some("main".into()),
                platform: "darwin".into(),
            },
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let de: BroadcastMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(de.raw_input, "这个月有节假日吗？");
    }
}
