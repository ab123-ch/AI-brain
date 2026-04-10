/// LLM 调用抽象
///
/// Phase 2 用 stub 实现，Phase 7 接入 Python MCP Server。
pub trait LlmProvider: Send + Sync {
    /// 调用 LLM，返回生成的文本
    ///
    /// - model: 模型名（如 "haiku"）
    /// - system_prompt: 系统提示词
    /// - user_input: 用户输入
    /// - max_tokens: 最大生成 token 数
    fn complete(
        &self,
        model: &str,
        system_prompt: &str,
        user_input: &str,
        max_tokens: u32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + '_>>;
}

/// 测试用 stub：直接返回固定格式的描述
pub struct StubLlmProvider {
    /// 固定返回前缀（模拟 LLM 解析）
    pub prefix: String,
}

impl StubLlmProvider {
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }
}

impl Default for StubLlmProvider {
    fn default() -> Self {
        Self::new("[感知解析] ")
    }
}

impl LlmProvider for StubLlmProvider {
    fn complete(
        &self,
        _model: &str,
        _system_prompt: &str,
        user_input: &str,
        _max_tokens: u32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + '_>>
    {
        let result = format!("{}{}", self.prefix, user_input);
        Box::pin(async move { Ok(result) })
    }
}

/// 失败 stub：模拟 LLM 不可用
pub struct FailingLlmProvider;

impl LlmProvider for FailingLlmProvider {
    fn complete(
        &self,
        _model: &str,
        _system_prompt: &str,
        _user_input: &str,
        _max_tokens: u32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + '_>>
    {
        Box::pin(async { Err("LLM service unavailable".into()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stub_llm() {
        let provider = StubLlmProvider::new("[test] ");
        let result = provider
            .complete("haiku", "sys", "hello", 100)
            .await
            .unwrap();
        assert_eq!(result, "[test] hello");
    }

    #[tokio::test]
    async fn test_failing_llm() {
        let provider = FailingLlmProvider;
        let result = provider.complete("haiku", "sys", "hello", 100).await;
        assert!(result.is_err());
    }
}
