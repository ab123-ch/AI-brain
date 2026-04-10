use std::future::Future;
use std::pin::Pin;

use crate::types::{
    BrainId, BrainKind, BrainResponse, BroadcastMessage, CollaborationMessage, ContextSnapshot,
    EvaluationResult, FastThinkResult, SlowThinkResult, ThinkContext,
};

/// 副脑必须实现的 trait
///
/// 每个副脑实现此 trait 后注册到消息总线，即可参与系统运行。
/// 快思考/慢思考由各副脑内部决定，外部只调用接口。
pub trait BrainAgent: Send + Sync {
    /// 副脑唯一标识
    fn id(&self) -> &BrainId;

    /// 副脑种类
    fn kind(&self) -> BrainKind;

    /// 快思考 — 本地规则/经验匹配，不调 LLM
    ///
    /// 延迟目标: ~10ms
    /// 返回: FastThinkResult（相关性、置信度、建议）
    fn fast_think(&self, msg: &BroadcastMessage) -> FastThinkResult;

    /// 慢思考 — 需要 LLM 调用
    ///
    /// 延迟目标: ~1-5s
    /// 由主脑通过协作通道调度，不是每次都触发
    fn slow_think(
        &self,
        msg: &BroadcastMessage,
        context: &ThinkContext,
    ) -> Pin<Box<dyn Future<Output = SlowThinkResult> + Send + '_>>;

    /// 接收广播消息（通道1）
    ///
    /// 各副脑内部决定是否响应：
    /// - `fast_think` 返回 relevant=true -> 提交结果到通道3
    /// - `fast_think` 返回 relevant=false -> 忽略或提交 `NotRelevant`
    #[allow(clippy::doc_markdown)]
    fn on_broadcast(&mut self, msg: BroadcastMessage);

    /// 接收协作消息（通道2）
    ///
    /// 场景：
    /// - 记忆脑收到推理脑的召回请求
    /// - 执行脑收到主脑的调度指令
    /// - 推理脑收到记忆脑的召回结果
    ///
    /// 返回 `Some(BrainResponse)` 表示产生出站消息（通过总线提交结果），
    /// 返回 `None` 表示不产生出站消息。
    fn on_collaboration(&mut self, msg: CollaborationMessage) -> Option<BrainResponse>;

    /// 启动时初始化（可选）
    fn on_activate(&mut self) {}

    /// 关闭时清理（可选）
    fn on_deactivate(&mut self) {}

    /// 慢思考完成后回调（可选）
    ///
    /// 默认不做任何事。各副脑可覆盖此方法来处理慢思考产出的新经验。
    /// 例如推理脑可在此处将 new_experience 写入经验库。
    fn on_slow_think_result(&mut self, _result: &SlowThinkResult) {}
}

/// 不参与快/慢思考循环的无状态副脑（评估脑）
///
/// 评估脑不维护上下文，每次执行后自毁。
/// 它的"智能"全在 system prompt 的规则编码中。
pub trait StatelessBrain: Send + Sync {
    fn id(&self) -> &BrainId;
    fn kind(&self) -> BrainKind;

    /// 执行评估，返回结果后建议立即 drop
    fn evaluate(&self, snapshots: Vec<ContextSnapshot>) -> EvaluationResult;
}
