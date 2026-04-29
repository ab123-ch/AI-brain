use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use brain_core::agent::BrainAgent;
use brain_core::types::{
    BrainId, BrainKind, BrainResponse, BroadcastMessage, CollaborationKind, CollaborationMessage,
    FastThinkResult, SlowThinkResult, ThinkContext, ToolCall, ToolDescriptor, ToolExecutionResult,
};
use brain_llm::{ChatMessage, ChatRequest, LlmProvider};

use crate::error::{MotorError, Result};
use crate::tool_registry::{ToolCapability, ToolRegistry};

/// 执行脑 LLM System Prompt
const MOTOR_SYSTEM_PROMPT: &str = "\
你是工具选择引擎。根据任务描述，从可用工具列表中选择最合适的工具组合。

输出格式：
第一行：结论（选择的工具，用 → 连接）
后续行：每行一个推理步骤

保持简洁。只输出必要的工具。";

/// 执行脑配置
#[derive(Debug, Clone)]
pub struct MotorConfig {
    /// 工具执行超时（毫秒）
    pub execution_timeout_ms: u64,
    /// 是否需要校验脑审核中高风险工具
    pub require_validation: bool,
}

impl Default for MotorConfig {
    fn default() -> Self {
        Self {
            execution_timeout_ms: 30_000,
            require_validation: true,
        }
    }
}

/// 执行脑（运动皮层）
///
/// 职责：
/// - 管理工具注册表，知道每个工具的能力和风险等级
/// - 执行经过校验脑审核的工具调用
/// - 快思考：判断消息是否涉及工具使用，推荐合适工具
/// - 慢思考：执行具体的工具调用
///
/// 安全约束：
/// - 只执行 `validated=true` 或低风险的工具
/// - 中高风险必须先经过校验脑审核
pub struct MotorBrain {
    id: BrainId,
    config: MotorConfig,
    registry: ToolRegistry,
    /// LLM Provider（可选，慢思考时用于智能工具选择）
    llm: Option<Arc<dyn LlmProvider>>,
    /// 待处理的工具调用
    #[allow(dead_code)]
    pending_calls: Vec<ToolCall>,
}

impl MotorBrain {
    pub fn new(config: MotorConfig) -> Result<Self> {
        Ok(Self {
            id: BrainId::motor(),
            config,
            registry: ToolRegistry::with_builtin_tools(),
            llm: None,
            pending_calls: Vec::new(),
        })
    }

    /// 注入 LLM Provider（由编排器在初始化时调用）
    pub fn set_llm(&mut self, llm: Arc<dyn LlmProvider>) {
        self.llm = Some(llm);
        tracing::info!("执行脑已接入 LLM");
    }

    /// 执行工具调用
    ///
    /// 前置条件：工具已注册，且已通过校验脑审核（中高风险）
    pub fn execute_tool(&mut self, tool_call: &ToolCall) -> Result<ToolExecutionResult> {
        let start = Instant::now();

        // 1. 检查工具是否已注册
        let capability = self
            .registry
            .get(&tool_call.tool_name)
            .ok_or_else(|| MotorError::ToolNotRegistered(tool_call.tool_name.clone()))?;

        // 2. 安全检查：中高风险必须经过校验
        if self.config.require_validation && capability.requires_validation && !tool_call.validated
        {
            return Err(MotorError::ValidationFailed(format!(
                "工具 \"{}\" 需要校验脑审核，但 tool_call.validated=false",
                tool_call.tool_name
            )));
        }

        // 3. 执行工具（当前为 stub，Phase 7 接入实际工具执行器）
        let output = self.execute_stub(tool_call, capability);
        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(ToolExecutionResult {
            tool_name: tool_call.tool_name.clone(),
            output,
            is_error: false,
            duration_ms,
        })
    }

    /// 获取所有可用工具描述
    pub fn list_tools(&self) -> Vec<ToolDescriptor> {
        self.registry
            .list()
            .iter()
            .map(|c| ToolDescriptor {
                name: c.name.clone(),
                description: c.description.clone(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            })
            .collect()
    }

    /// 按场景搜索工具
    pub fn search_tools(&self, scenario: &str) -> Vec<&ToolCapability> {
        self.registry.search_by_scenario(scenario)
    }

    /// 获取工具的风险等级
    pub fn tool_risk_level(&self, name: &str) -> crate::tool_registry::ToolRiskLevel {
        self.registry.risk_level(name)
    }

    /// 从广播消息中提取工具相关关键词
    fn extract_tool_hints(&self, content: &str) -> Vec<String> {
        let content_lower = content.to_lowercase();
        let mut hints = Vec::new();

        // 工具相关关键词映射
        let keyword_map = [
            ("文件", vec!["Read", "Edit", "Write", "Glob"]),
            ("读取", vec!["Read"]),
            ("查看", vec!["Read", "Glob"]),
            ("搜索", vec!["Grep", "Glob", "WebSearch"]),
            ("查找", vec!["Grep", "Glob"]),
            ("编辑", vec!["Edit"]),
            ("修改", vec!["Edit"]),
            ("创建", vec!["Write"]),
            ("写入", vec!["Write"]),
            ("执行", vec!["Bash"]),
            ("命令", vec!["Bash"]),
            ("网络", vec!["WebSearch"]),
            ("代码", vec!["Read", "Edit", "Grep", "LSP"]),
            ("定义", vec!["LSP"]),
            ("引用", vec!["LSP"]),
            ("运行", vec!["Bash"]),
            ("测试", vec!["Bash", "Grep"]),
        ];

        for (keyword, tools) in &keyword_map {
            if content_lower.contains(keyword) {
                for tool in tools {
                    if !hints.contains(&tool.to_string()) {
                        hints.push(tool.to_string());
                    }
                }
            }
        }

        hints
    }

    /// Stub 工具执行（Phase 7 接入实际执行器）
    fn execute_stub(&self, call: &ToolCall, _capability: &ToolCapability) -> String {
        format!(
            "[stub] 工具 \"{}\" 执行完成，输入: {}",
            call.tool_name,
            if call.input.to_string().len() > 200 {
                let truncated: String = call.input.to_string().chars().take(200).collect();
                format!("{truncated}...")
            } else {
                call.input.to_string()
            }
        )
    }

    /// 获取注册工具数
    pub fn tool_count(&self) -> usize {
        self.registry.len()
    }

    // ─── 私有方法 ──────────────────────────────────────────────

    /// 生成工具列表描述（给 LLM prompt 用）
    fn list_tool_descriptions(&self) -> String {
        self.registry
            .list()
            .iter()
            .map(|c| format!("- {}: {} (风险: {:?})", c.name, c.description, c.risk_level))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// LLM 智能工具选择
    async fn select_tools_with_llm(
        provider: &Arc<dyn LlmProvider>,
        task: &str,
        tools_desc: &str,
    ) -> std::result::Result<(String, Vec<String>), String> {
        let request = ChatRequest {
            model: None,
            messages: vec![
                ChatMessage::system(MOTOR_SYSTEM_PROMPT),
                ChatMessage::user(format!("## 任务\n{task}\n\n## 可用工具\n{tools_desc}")),
            ],
            max_tokens: Some(1024),
            temperature: Some(0.3),
            tools: None,
            tool_choice: None,
        };

        let response = provider
            .complete(request)
            .await
            .map_err(|e| e.to_string())?;
        let content = response.text();

        // 解析：第一行是结论，后续是推理路径
        let mut lines = content.lines().peekable();
        let conclusion = lines.next().unwrap_or("工具选择完成").to_string();
        let reasoning_path: Vec<String> = lines.map(std::string::ToString::to_string).collect();

        Ok((conclusion, reasoning_path))
    }

}

impl BrainAgent for MotorBrain {
    fn id(&self) -> &BrainId {
        &self.id
    }

    fn kind(&self) -> BrainKind {
        BrainKind::Motor
    }

    /// 快思考 — 判断消息是否涉及工具使用，推荐合适工具
    fn fast_think(&self, msg: &BroadcastMessage) -> FastThinkResult {
        let tool_hints = self.extract_tool_hints(&msg.content);

        if tool_hints.is_empty() {
            // 消息不涉及工具使用
            return FastThinkResult {
                relevant: false,
                confidence: 0.3,
                summary: Some("消息不涉及工具操作".into()),
                suggested_tools: Vec::new(),
                matched_experience: None,
            };
        }

        FastThinkResult {
            relevant: true,
            confidence: 0.8,
            summary: Some(format!("识别到工具需求: {}", tool_hints.join(", "))),
            suggested_tools: tool_hints,
            matched_experience: None,
        }
    }

    /// 慢思考 — 执行具体的工具调用
    ///
    /// 需要接入 LLM 才能执行智能工具选择。
    /// LLM 不可用或调用失败时返回低置信度结果（非降级）。
    fn slow_think(
        &self,
        msg: &BroadcastMessage,
        _context: &ThinkContext,
    ) -> Pin<Box<dyn Future<Output = SlowThinkResult> + Send + '_>> {
        let content = msg.content.clone();
        let llm = self.llm.clone();
        let tools_desc = self.list_tool_descriptions();

        Box::pin(async move {
            let provider = match llm {
                Some(p) => p,
                None => {
                    tracing::error!("执行脑慢思考失败: 未接入 LLM Provider");
                    return SlowThinkResult {
                        conclusion: "执行脑未接入 LLM，无法进行慢思考".into(),
                        reasoning_path: Vec::new(),
                        confidence: 0.0,
                        sources: Vec::new(),
                        new_experience: None,
                    };
                }
            };

            match Self::select_tools_with_llm(&provider, &content, &tools_desc).await {
                Ok((conclusion, path)) => SlowThinkResult {
                    conclusion,
                    reasoning_path: path,
                    confidence: 0.85,
                    sources: vec![brain_core::types::KnowledgeSource::LlmReasoning {
                        model: provider.model().into(),
                    }],
                    new_experience: None,
                },
                Err(e) => {
                    tracing::error!("执行脑 LLM 调用失败: {e}");
                    SlowThinkResult {
                        conclusion: format!("执行脑 LLM 调用失败: {e}"),
                        reasoning_path: Vec::new(),
                        confidence: 0.0,
                        sources: Vec::new(),
                        new_experience: None,
                    }
                }
            }
        })
    }

    fn on_broadcast(&mut self, msg: BroadcastMessage) {
        let result = self.fast_think(&msg);
        if result.relevant && !result.suggested_tools.is_empty() {
            tracing::debug!("执行脑识别工具需求: {:?}", result.suggested_tools);
            // 记录待处理的工具需求
        }
    }

    fn on_collaboration(&mut self, msg: CollaborationMessage) -> Option<BrainResponse> {
        match msg.kind {
            CollaborationKind::Dispatch => {
                if msg.to.contains(&self.id) {
                    tracing::debug!(
                        "执行脑收到主脑调度: {}",
                        msg.content.chars().take(100).collect::<String>()
                    );
                }
            }
            CollaborationKind::Request | CollaborationKind::Response => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::BrainContext;
    use chrono::Utc;

    fn make_brain() -> MotorBrain {
        MotorBrain::new(MotorConfig::default()).unwrap()
    }

    fn make_broadcast(content: &str) -> BroadcastMessage {
        BroadcastMessage {
            content: content.into(),
            raw_input: content.into(),
            context: BrainContext {
                current_date: "2026-04-04".into(),
                cwd: "/test".into(),
                git_branch: Some("main".into()),
                platform: "darwin".into(),
            },
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn brain_id_and_kind() {
        let brain = make_brain();
        assert_eq!(brain.id(), &BrainId::motor());
        assert_eq!(brain.kind(), BrainKind::Motor);
    }

    #[test]
    fn builtin_tools_registered() {
        let brain = make_brain();
        assert!(brain.tool_count() >= 9);
    }

    #[test]
    fn fast_think_file_operation() {
        let brain = make_brain();
        let msg = make_broadcast("帮我读取并查看这个文件的内容");
        let result = brain.fast_think(&msg);
        assert!(result.relevant);
        assert!(result.suggested_tools.contains(&"Read".to_string()));
    }

    #[test]
    fn fast_think_search_operation() {
        let brain = make_brain();
        let msg = make_broadcast("搜索代码中的错误");
        let result = brain.fast_think(&msg);
        assert!(result.relevant);
        assert!(result.suggested_tools.contains(&"Grep".to_string()));
    }

    #[test]
    fn fast_think_not_relevant() {
        let brain = make_brain();
        let msg = make_broadcast("今天天气怎么样？");
        let result = brain.fast_think(&msg);
        assert!(!result.relevant);
        assert!(result.suggested_tools.is_empty());
    }

    #[test]
    fn execute_low_risk_tool() {
        let mut brain = make_brain();
        let call = ToolCall {
            tool_name: "Read".into(),
            input: serde_json::Value::String("/path/to/file".into()),
            validated: false, // 低风险不需要校验
            validation_id: None,
        };
        let result = brain.execute_tool(&call).unwrap();
        assert_eq!(result.tool_name, "Read");
        assert!(!result.is_error);
    }

    #[test]
    fn execute_medium_risk_without_validation_fails() {
        let mut brain = make_brain();
        let call = ToolCall {
            tool_name: "Edit".into(),
            input: serde_json::Value::String("修改文件".into()),
            validated: false,
            validation_id: None,
        };
        let result = brain.execute_tool(&call);
        assert!(result.is_err());
    }

    #[test]
    fn execute_medium_risk_with_validation_succeeds() {
        let mut brain = make_brain();
        let call = ToolCall {
            tool_name: "Edit".into(),
            input: serde_json::Value::String("修改文件".into()),
            validated: true,
            validation_id: Some("val_001".into()),
        };
        let result = brain.execute_tool(&call).unwrap();
        assert!(!result.is_error);
    }

    #[test]
    fn execute_unregistered_tool_fails() {
        let mut brain = make_brain();
        let call = ToolCall {
            tool_name: "NonExistentTool".into(),
            input: serde_json::Value::Null,
            validated: true,
            validation_id: None,
        };
        let result = brain.execute_tool(&call);
        assert!(result.is_err());
    }

    #[test]
    fn list_tools_returns_descriptors() {
        let brain = make_brain();
        let tools = brain.list_tools();
        assert!(tools.len() >= 9);
        assert!(tools.iter().any(|t| t.name == "Read"));
        assert!(tools.iter().any(|t| t.name == "Bash"));
    }

    #[test]
    fn search_tools_by_scenario() {
        let brain = make_brain();
        let results = brain.search_tools("文件");
        assert!(results.len() >= 3);
    }

    #[test]
    fn skip_validation_config() {
        let mut brain = MotorBrain::new(MotorConfig {
            require_validation: false,
            ..Default::default()
        })
        .unwrap();
        let call = ToolCall {
            tool_name: "Edit".into(),
            input: serde_json::Value::String("修改".into()),
            validated: false,
            validation_id: None,
        };
        // require_validation=false → 不检查 validated 字段
        let result = brain.execute_tool(&call).unwrap();
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn slow_think_without_llm_returns_low_confidence() {
        let brain = make_brain();
        let msg = make_broadcast("帮我搜索代码并编辑文件");
        let result = brain
            .slow_think(
                &msg,
                &ThinkContext {
                    related_memories: Vec::new(),
                    task_history: Vec::new(),
                },
            )
            .await;
        // 无 LLM 时返回低置信度结果
        assert!((result.confidence - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn on_broadcast_captures_tool_needs() {
        let mut brain = make_brain();
        let msg = make_broadcast("运行测试命令");
        brain.on_broadcast(msg);
        // 不 panic 即可
    }

    #[test]
    fn on_collaboration_dispatch() {
        let mut brain = make_brain();
        let msg = CollaborationMessage {
            id: "collab_001".into(),
            from: BrainId::master(),
            to: vec![BrainId::motor()],
            correlation_id: None,
            hop_count: 0,
            priority: brain_core::types::MessagePriority::Normal,
            content: "执行工具调用".into(),
            kind: CollaborationKind::Dispatch,
        };
        brain.on_collaboration(msg);
    }

    #[test]
    fn tool_risk_level_query() {
        let brain = make_brain();
        assert_eq!(
            brain.tool_risk_level("Read"),
            crate::tool_registry::ToolRiskLevel::Low
        );
        assert_eq!(
            brain.tool_risk_level("Edit"),
            crate::tool_registry::ToolRiskLevel::Medium
        );
        assert_eq!(
            brain.tool_risk_level("Bash"),
            crate::tool_registry::ToolRiskLevel::High
        );
    }
}
