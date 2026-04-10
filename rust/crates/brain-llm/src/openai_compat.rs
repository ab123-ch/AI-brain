use serde::{Deserialize, Serialize};

use crate::error::{LlmError, Result};
use crate::provider::{ChatRequest, ChatResponse, LlmProvider, TokenUsage};

/// OpenAI 兼容 API 客户端
///
/// 支持 GLM（智谱）、OpenAI、本地模型（Ollama/vLLM）等
/// 所有兼容 `/v1/chat/completions` 的 Provider 通用
pub struct OpenAiCompatClient {
    api_base: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f64,
    client: reqwest::Client,
}

/// API 请求格式（OpenAI 兼容）
#[derive(Debug, Serialize)]
struct ApiChatRequest {
    model: String,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ApiMessage {
    role: String,
    content: String,
}

/// API 响应格式（OpenAI 兼容）
#[derive(Debug, Deserialize)]
struct ApiChatResponse {
    choices: Vec<ApiChoice>,
    model: Option<String>,
    usage: Option<ApiUsage>,
}

#[derive(Debug, Deserialize)]
struct ApiChoice {
    message: Option<ApiMessage>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(clippy::struct_field_names)]
struct ApiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl OpenAiCompatClient {
    pub fn new(
        api_base: String,
        api_key: String,
        model: String,
        max_tokens: u32,
        temperature: f64,
    ) -> Self {
        Self {
            api_base,
            api_key,
            model,
            max_tokens,
            temperature,
            client: reqwest::Client::new(),
        }
    }

    /// 构建 API URL
    fn chat_url(&self) -> String {
        let base = self.api_base.trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    /// 转换 ChatRequest → API 请求体
    fn to_api_request(&self, request: ChatRequest) -> ApiChatRequest {
        let messages: Vec<ApiMessage> = request
            .messages
            .into_iter()
            .map(|m| ApiMessage {
                role: match m.role {
                    crate::provider::MessageRole::System => "system".into(),
                    crate::provider::MessageRole::User => "user".into(),
                    crate::provider::MessageRole::Assistant => "assistant".into(),
                },
                content: m.content,
            })
            .collect();

        ApiChatRequest {
            model: request.model.unwrap_or_else(|| self.model.clone()),
            messages,
            max_tokens: request.max_tokens.or(Some(self.max_tokens)),
            temperature: request.temperature.or(Some(self.temperature)),
            stream: Some(false),
        }
    }
}

impl LlmProvider for OpenAiCompatClient {
    fn model(&self) -> &str {
        &self.model
    }

    fn complete(
        &self,
        request: ChatRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ChatResponse>> + Send + '_>> {
        let api_request = self.to_api_request(request);
        let url = self.chat_url();
        let api_key = self.api_key.clone();

        Box::pin(async move {
            let response = self.client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&api_request)
                .send()
                .await
                .map_err(|e| LlmError::RequestFailed(format!("HTTP 请求失败: {e}")))?;

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(LlmError::ApiError {
                    status: status.as_u16(),
                    message: body,
                });
            }

            let api_resp: ApiChatResponse = response
                .json()
                .await
                .map_err(|e| LlmError::RequestFailed(format!("解析响应失败: {e}")))?;

            let content = api_resp
                .choices
                .first()
                .and_then(|c| c.message.as_ref())
                .map(|m| m.content.clone())
                .unwrap_or_default();

            let usage = api_resp.usage.map_or(TokenUsage::default(), |u| TokenUsage {
                prompt_tokens: u.prompt_tokens.unwrap_or(0),
                completion_tokens: u.completion_tokens.unwrap_or(0),
                total_tokens: u.total_tokens.unwrap_or(0),
            });

            Ok(ChatResponse {
                content,
                model: api_resp.model.unwrap_or(api_request.model),
                usage,
                finish_reason: api_resp
                    .choices
                    .first()
                    .and_then(|c| c.finish_reason.clone()),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ChatMessage;

    fn make_client() -> OpenAiCompatClient {
        OpenAiCompatClient::new(
            "https://open.bigmodel.cn/api/paas/v4".into(),
            "test-key".into(),
            "glm-4.7".into(),
            4096,
            0.7,
        )
    }

    #[test]
    fn client_model() {
        let client = make_client();
        assert_eq!(client.model(), "glm-4.7");
    }

    #[test]
    fn chat_url_construction() {
        let client = make_client();
        assert_eq!(
            client.chat_url(),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
    }

    #[test]
    fn chat_url_no_trailing_slash() {
        let client = OpenAiCompatClient::new(
            "https://api.example.com/v1/".into(),
            "key".into(),
            "test".into(),
            1024,
            0.5,
        );
        assert_eq!(client.chat_url(), "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn to_api_request_with_override() {
        let client = make_client();
        let chat_req = ChatRequest {
            model: Some("glm-5.1".into()),
            messages: vec![
                ChatMessage::system("你是助手"),
                ChatMessage::user("你好"),
            ],
            max_tokens: Some(2048),
            temperature: Some(0.3),
            stream: None,
        };
        let api_req = client.to_api_request(chat_req);
        assert_eq!(api_req.model, "glm-5.1");
        assert_eq!(api_req.messages.len(), 2);
        assert_eq!(api_req.messages[0].role, "system");
        assert_eq!(api_req.messages[1].role, "user");
        assert_eq!(api_req.max_tokens, Some(2048));
        assert_eq!(api_req.temperature, Some(0.3));
    }

    #[test]
    fn to_api_request_uses_defaults() {
        let client = make_client();
        let chat_req = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("测试")],
            max_tokens: None,
            temperature: None,
            stream: None,
        };
        let api_req = client.to_api_request(chat_req);
        assert_eq!(api_req.model, "glm-4.7");
        assert_eq!(api_req.max_tokens, Some(4096));
    }

    #[test]
    fn parse_api_response() {
        let json = r#"{
            "choices": [
                {
                    "message": {"role": "assistant", "content": "你好！有什么可以帮助你的？"},
                    "finish_reason": "stop"
                }
            ],
            "model": "glm-4.7",
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 15,
                "total_tokens": 25
            }
        }"#;

        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.as_ref().unwrap().content, "你好！有什么可以帮助你的？");
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, Some(25));
    }

    #[test]
    fn parse_empty_choices() {
        let json = r#"{"choices": [], "model": "test"}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.choices.is_empty());
    }

    #[test]
    fn parse_response_no_usage() {
        let json = r#"{
            "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "model": "glm-5-turbo"
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage.is_none());
    }
}
