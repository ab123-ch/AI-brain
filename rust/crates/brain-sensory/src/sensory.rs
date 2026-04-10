use std::sync::Arc;

use brain_bus::BrainBus;
use brain_core::types::{BrainContext, BrainId, BrainKind, BroadcastMessage};

use crate::context::gather_context;
use crate::error::SensoryError;
use crate::llm::LlmProvider;

/// 感知脑 — 系统唯一入口
///
/// 接收外部输入 → LLM 解析为自然语言描述 → 注入环境上下文 → 投递广播。
/// 不实现 BrainAgent trait，因为它是系统的入口而非副脑。
pub struct SensoryBrain {
    id: BrainId,
    bus: Arc<BrainBus>,
    llm: Box<dyn LlmProvider>,
    model: String,
    system_prompt: String,
}

impl SensoryBrain {
    /// 创建感知脑
    ///
    /// - model: 用于解析的模型名（建议 "haiku"）
    /// - bus: 共享的消息总线（Arc 包装）
    /// - llm: LLM 调用实现
    pub fn new(model: &str, bus: Arc<BrainBus>, llm: Box<dyn LlmProvider>) -> Self {
        Self {
            id: BrainId::sensory(),
            bus,
            llm,
            model: model.into(),
            system_prompt: build_system_prompt(),
        }
    }

    /// 接收外部输入，LLM 解析后投递广播
    ///
    /// 流程:
    ///   1. 接收 raw_input
    ///   2. 构建环境上下文 (date, cwd, git_branch)
    ///   3. 调用 LLM 生成自然语言描述
    ///   4. 投递到广播通道
    ///   5. 降级: LLM 失败 → 直接投递原始输入 + 标记 "未解析"
    pub async fn process_input(&self, raw_input: &str) -> Result<String, SensoryError> {
        let context = gather_context();
        let content = self.parse_with_llm(raw_input, &context).await;

        let msg = BroadcastMessage {
            content: content.clone(),
            raw_input: raw_input.into(),
            context,
            timestamp: chrono::Utc::now(),
        };

        self.bus.broadcast(msg)?;
        Ok(content)
    }

    /// LLM 解析，降级策略
    async fn parse_with_llm(&self, raw_input: &str, context: &BrainContext) -> String {
        let prompt = format!(
            "当前环境: 日期={}, 目录={}, 平台={}\n用户输入: {}",
            context.current_date, context.cwd, context.platform, raw_input
        );

        match self
            .llm
            .complete(&self.model, &self.system_prompt, &prompt, 512)
            .await
        {
            Ok(parsed) => parsed,
            Err(e) => {
                tracing::warn!("LLM 解析失败，降级为原始输入: {e}");
                format!("[未解析] {raw_input}")
            }
        }
    }

    pub fn id(&self) -> &BrainId {
        &self.id
    }

    pub fn kind(&self) -> BrainKind {
        BrainKind::Sensory
    }
}

fn build_system_prompt() -> String {
    "你是感知模块，负责将用户的原始输入解析为结构化的自然语言描述。\
     提取关键意图、涉及的实体和操作。保持简洁，1-3句话。\
     如果输入包含代码或技术问题，标注涉及的语言和工具。"
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::StubLlmProvider;

    fn make_bus() -> Arc<BrainBus> {
        Arc::new(BrainBus::new(16, 16, 16))
    }

    #[tokio::test]
    async fn test_process_input_success() {
        let bus = make_bus();
        let mut broadcast_rx = bus.subscribe_broadcast();

        let sensory = SensoryBrain::new("haiku", bus.clone(), Box::new(StubLlmProvider::default()));

        let result = sensory.process_input("这个月有节假日吗？").await.unwrap();
        assert!(result.contains("这个月有节假日吗？"));

        // 验证广播投递成功
        let msg = broadcast_rx.recv().await.unwrap();
        assert_eq!(msg.raw_input, "这个月有节假日吗？");
        assert!(!msg.context.current_date.is_empty());
    }

    #[tokio::test]
    async fn test_llm_failure_degrades_gracefully() {
        let bus = make_bus();
        let mut broadcast_rx = bus.subscribe_broadcast();

        let sensory = SensoryBrain::new(
            "haiku",
            bus.clone(),
            Box::new(crate::llm::FailingLlmProvider),
        );
        drop(bus);

        let result = sensory.process_input("测试降级").await.unwrap();
        assert!(result.contains("[未解析]"));

        let msg = broadcast_rx.recv().await.unwrap();
        assert_eq!(msg.content, "[未解析] 测试降级");
    }
}
