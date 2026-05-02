use crate::evolution_engine::EvolutionEngine;
use brain_core::agent::BrainAgent;
use brain_core::types::*;
use brain_llm::LlmProvider;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

/// 进化脑 -- 实现 BrainAgent trait
pub struct EvolverBrain {
    id: BrainId,
    engine: Arc<Mutex<EvolutionEngine>>,
    #[allow(dead_code)] // 后续 LLM 驱动进化循环使用
    llm: Arc<dyn LlmProvider>,
}

impl EvolverBrain {
    pub fn new(llm: Arc<dyn LlmProvider>, repo_path: &std::path::Path) -> Self {
        let engine = EvolutionEngine::new(llm.clone(), repo_path);
        Self {
            id: BrainId::evolver(),
            engine: Arc::new(Mutex::new(engine)),
            llm,
        }
    }

    pub fn engine(&self) -> Arc<Mutex<EvolutionEngine>> {
        self.engine.clone()
    }
}

impl BrainAgent for EvolverBrain {
    fn id(&self) -> &BrainId {
        &self.id
    }

    fn kind(&self) -> BrainKind {
        BrainKind::Evolver
    }

    fn fast_think(&self, msg: &BroadcastMessage) -> FastThinkResult {
        let content = &msg.content;
        let keywords = [
            "优化",
            "进化",
            "重构",
            "evolve",
            "optimize",
            "refactor",
            "自我改进",
        ];
        let relevant = keywords.iter().any(|k| content.to_lowercase().contains(k));

        FastThinkResult {
            relevant,
            confidence: if relevant { 0.8 } else { 0.1 },
            summary: if relevant {
                Some("触发进化脑处理".into())
            } else {
                None
            },
            suggested_tools: Vec::new(),
            matched_experience: None,
        }
    }

    fn slow_think(
        &self,
        msg: &BroadcastMessage,
        _context: &ThinkContext,
    ) -> Pin<Box<dyn Future<Output = SlowThinkResult> + Send + '_>> {
        let content = msg.content.clone();
        Box::pin(async move {
            SlowThinkResult {
                conclusion: format!("进化脑收到任务: {content}"),
                reasoning_path: vec!["快思考匹配关键词".into(), "准备启动进化流程".into()],
                confidence: 0.7,
                sources: vec![],
                new_experience: None,
            }
        })
    }

    fn on_broadcast(&mut self, _msg: BroadcastMessage) {
        // 进化脑主要通过协作通道工作
    }

    fn on_collaboration(&mut self, _msg: CollaborationMessage) -> Option<BrainResponse> {
        Some(BrainResponse {
            from: BrainId::evolver(),
            relevance: 0.7,
            confidence: 0.7,
            result: BrainResponsePayload::Processing,
            need_slow_think: false,
            timestamp: chrono::Utc::now(),
        })
    }
}
