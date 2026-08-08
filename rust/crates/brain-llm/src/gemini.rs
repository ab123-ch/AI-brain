//! Gemini 原生 API 客户端 — 走 generateContent / streamGenerateContent。
//!
//! 与 OpenAI 兼容路径并存的独立 Client，实现 LlmProvider trait。
//! 协议差异（systemInstruction 独立字段、functionCall 无 id、thought part 等）
//! 全部在本模块内部消化，上层零感知。

use std::future::Future;
use std::pin::Pin;

use serde_json::{json, Value};

use crate::error::{LlmError, Result};
use crate::http_client::SharedHttpClient;
use crate::openai_compat::RetryConfig;
use crate::provider::{ChatRequest, ChatResponse, LlmProvider, MessageRole};
use crate::stream;
use crate::types::{ContentBlock, FinishReason, StreamEvent, TokenUsage, ToolChoice};

pub struct GeminiClient {
    api_base: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f64,
    http: SharedHttpClient,
}

impl GeminiClient {
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        api_base: String,
        api_key: String,
        model: String,
        max_tokens: u32,
        temperature: f64,
        proxy_url: Option<String>,
    ) -> Self {
        Self::try_new(api_base, api_key, model, max_tokens, temperature, proxy_url)
            .expect("SharedHttpClient 构造失败")
    }

    /// Fallible constructor used when proxy settings come from user configuration.
    #[allow(clippy::needless_pass_by_value)]
    pub fn try_new(
        api_base: String,
        api_key: String,
        model: String,
        max_tokens: u32,
        temperature: f64,
        proxy_url: Option<String>,
    ) -> Result<Self> {
        let http = SharedHttpClient::new(proxy_url.as_deref(), RetryConfig::default())?;
        Ok(Self {
            api_base,
            api_key,
            model,
            max_tokens,
            temperature,
            http,
        })
    }

    /// 非流式端点
    fn generate_url(&self) -> String {
        let base = self.api_base.trim_end_matches('/');
        format!("{base}/models/{}:generateContent", self.model)
    }

    /// 流式端点（SSE）
    fn stream_url(&self) -> String {
        let base = self.api_base.trim_end_matches('/');
        format!("{base}/models/{}:streamGenerateContent?alt=sse", self.model)
    }

    fn build_headers(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Content-Type", "application/json".into()),
            ("x-goog-api-key", self.api_key.clone()),
        ]
    }

    /// 将 ChatRequest 转换为 Gemini 请求体
    #[allow(clippy::too_many_lines)]
    pub(crate) fn to_gemini_request(&self, request: ChatRequest) -> Value {
        let mut id_to_name = std::collections::HashMap::new();
        for msg in &request.messages {
            for block in &msg.content {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    id_to_name.insert(id.as_str(), name.as_str());
                }
            }
        }

        let mut system_parts = Vec::new();
        let mut contents = Vec::new();

        for msg in &request.messages {
            if msg.role == MessageRole::System {
                let text = msg.text_content();
                if !text.is_empty() {
                    system_parts.push(text);
                }
                continue;
            }

            let has_tool_use = msg.content.iter().any(ContentBlock::is_tool_use);
            let has_tool_result = msg.content.iter().any(ContentBlock::is_tool_result);
            let parts: Vec<Value> = if has_tool_result {
                msg.content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => id_to_name.get(tool_use_id.as_str()).map(|name| {
                            json!({
                                "functionResponse": {
                                    "name": name,
                                    "response": {"output": content},
                                }
                            })
                        }),
                        _ => None,
                    })
                    .collect()
            } else if has_tool_use {
                msg.content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(json!({"text": text})),
                        ContentBlock::ToolUse { id, name, input } => {
                            let mut part = json!({"functionCall": {"name": name, "args": input}});
                            if let Some(signature) = thought_signature_from_tool_call_id(id) {
                                part["thoughtSignature"] = json!(signature);
                            }
                            Some(part)
                        }
                        ContentBlock::Thinking { .. } | ContentBlock::ToolResult { .. } => None,
                    })
                    .collect()
            } else {
                vec![json!({"text": msg.text_content()})]
            };

            if parts.is_empty() {
                continue;
            }

            let role = if has_tool_result {
                "user"
            } else if msg.role == MessageRole::Assistant {
                "model"
            } else {
                "user"
            };
            contents.push(json!({"role": role, "parts": parts}));
        }

        let mut body = json!({ "contents": contents });
        if !system_parts.is_empty() {
            body["systemInstruction"] = json!({
                "parts": [{"text": system_parts.join("\n\n")}],
            });
        }

        if let Some(tools) = request.tools.as_ref() {
            if !tools.is_empty() {
                let declarations: Vec<Value> = tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "parametersJsonSchema": tool.input_schema,
                        })
                    })
                    .collect();
                body["tools"] = json!([{"functionDeclarations": declarations}]);
            }
        }

        if let Some(tool_choice) = request.tool_choice {
            let function_calling_config = match tool_choice {
                ToolChoice::Auto => json!({"mode": "AUTO"}),
                ToolChoice::None => json!({"mode": "NONE"}),
                ToolChoice::Tool { name } => {
                    json!({"mode": "ANY", "allowedFunctionNames": [name]})
                }
            };
            body["toolConfig"] = json!({"functionCallingConfig": function_calling_config});
        }

        body["generationConfig"] = json!({
            "maxOutputTokens": request.max_tokens.unwrap_or(self.max_tokens),
            "temperature": request.temperature.unwrap_or(self.temperature),
        });
        body
    }

    /// 将 Gemini 响应体解析为统一的 ChatResponse。
    pub(crate) fn parse_gemini_response(body: &Value, fallback_model: String) -> ChatResponse {
        let mut content = Vec::new();
        let mut finish_reason = None;
        let mut call_seq = 0;

        if let Some(first) = body
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
        {
            if let Some(parts) = first
                .get("content")
                .and_then(|candidate| candidate.get("parts"))
                .and_then(Value::as_array)
            {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        if part
                            .get("thought")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            content.push(ContentBlock::Thinking {
                                content: text.to_string(),
                            });
                        } else {
                            content.push(ContentBlock::Text {
                                text: text.to_string(),
                            });
                        }
                    }

                    if let Some(function_call) = part.get("functionCall") {
                        if let Some(name) = function_call.get("name").and_then(Value::as_str) {
                            call_seq += 1;
                            content.push(ContentBlock::ToolUse {
                                id: gemini_tool_call_id(
                                    call_seq,
                                    part.get("thoughtSignature").and_then(Value::as_str),
                                ),
                                name: name.to_string(),
                                input: function_call
                                    .get("args")
                                    .cloned()
                                    .unwrap_or_else(|| json!({})),
                            });
                        }
                    }
                }
            }

            if let Some(reason) = first.get("finishReason").and_then(Value::as_str) {
                finish_reason = Some(Self::finish_reason_from_str(reason, &content));
            }
        }

        let usage = body
            .get("usageMetadata")
            .map(|metadata| TokenUsage {
                prompt_tokens: metadata
                    .get("promptTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                completion_tokens: metadata
                    .get("candidatesTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                total_tokens: metadata
                    .get("totalTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            })
            .unwrap_or_default();

        ChatResponse {
            content,
            model: fallback_model,
            usage,
            finish_reason,
        }
    }

    fn finish_reason_from_str(reason: &str, content: &[ContentBlock]) -> FinishReason {
        let has_tool_use = content.iter().any(ContentBlock::is_tool_use);
        match reason {
            "MAX_TOKENS" => FinishReason::MaxTokens,
            "STOP" if has_tool_use => FinishReason::ToolUse,
            "STOP" => FinishReason::EndTurn,
            _ if has_tool_use => FinishReason::ToolUse,
            _ => FinishReason::EndTurn,
        }
    }
}

pub(crate) fn gemini_tool_call_id(sequence: usize, thought_signature: Option<&str>) -> String {
    thought_signature.map_or_else(
        || format!("call_{sequence}"),
        |signature| {
            format!(
                "call_{sequence}__gemini_thought_{}",
                hex_encode(signature.as_bytes())
            )
        },
    )
}

fn thought_signature_from_tool_call_id(id: &str) -> Option<String> {
    let (_, encoded) = id.split_once("__gemini_thought_")?;
    let bytes = hex_decode(encoded)?;
    String::from_utf8(bytes).ok()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn hex_decode(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            u8::try_from((high << 4) | low).ok()
        })
        .collect()
}

impl LlmProvider for GeminiClient {
    fn model(&self) -> &str {
        &self.model
    }

    fn complete(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>> {
        let url = self.generate_url();
        let body = self.to_gemini_request(request);
        let headers = self.build_headers();
        let fallback_model = self.model.clone();
        let retry = self.http.retry_config().clone();
        let client = self.http.client().clone();

        Box::pin(async move {
            let mut attempts = 0;
            let max_attempts = retry.max_retries + 1;
            loop {
                attempts += 1;
                tracing::info!("Gemini 请求发送开始: url={url}, 第{attempts}次尝试");
                let mut request = client.post(&url);
                for (name, value) in &headers {
                    request = request.header(*name, value);
                }

                match request.json(&body).send().await {
                    Ok(response) => {
                        let status = response.status();
                        tracing::info!("Gemini 请求收到响应: url={url}, status={status}");
                        if !status.is_success() {
                            let body = response.text().await.unwrap_or_default();
                            let body_bytes = body.len();
                            let error = LlmError::ApiError {
                                status: status.as_u16(),
                                message: body,
                            };
                            if error.is_retryable() && attempts < max_attempts {
                                let backoff = retry.backoff_for_attempt(attempts);
                                tracing::warn!(
                                    "Gemini 返回可重试错误: url={url}, status={status}, \
                                     第{attempts}次尝试, {backoff:?}后重试"
                                );
                                tokio::time::sleep(backoff).await;
                                continue;
                            }
                            tracing::error!(
                                "Gemini API 非成功响应: url={url}, status={status}, body_bytes={body_bytes}; 响应体不写入日志"
                            );
                            return Err(error);
                        }

                        let value = response.json::<Value>().await.map_err(|e| {
                            LlmError::RequestFailed(format!("Gemini 响应解析失败: {e}"))
                        })?;
                        return Ok(Self::parse_gemini_response(&value, fallback_model));
                    }
                    Err(source) => {
                        let error =
                            LlmError::RequestFailed(format!("Gemini HTTP 请求失败: {source}"));
                        if error.is_retryable() && attempts < max_attempts {
                            let backoff = retry.backoff_for_attempt(attempts);
                            tracing::warn!(
                                "Gemini 请求发送失败（可重试）: url={url}, \
                                 第{attempts}次尝试, error={source}, {backoff:?}后重试"
                            );
                            tokio::time::sleep(backoff).await;
                            continue;
                        }
                        tracing::error!("Gemini 请求发送失败: url={url}, error={source}");
                        return Err(error);
                    }
                }
            }
        })
    }

    fn stream_complete(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<StreamEvent>>> + Send + '_>> {
        let url = self.stream_url();
        let body = self.to_gemini_request(request);
        let headers = self.build_headers();
        Box::pin(async move {
            tracing::info!("Gemini 批量流式请求发送开始: url={url}");
            let result = stream::stream_gemini(self.http.client(), &url, &headers, &body).await;
            match &result {
                Ok(events) => tracing::info!(
                    "Gemini 批量流式请求完成: url={url}, events={}",
                    events.len()
                ),
                Err(error) => {
                    tracing::error!("Gemini 批量流式请求失败: url={url}, error={error}");
                }
            }
            result
        })
    }

    fn stream_incremental(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<tokio::sync::mpsc::Receiver<StreamEvent>>> + Send + '_>>
    {
        let url = self.stream_url();
        let body = self.to_gemini_request(request);
        let headers = self.build_headers();
        Box::pin(async move {
            tracing::info!("Gemini 增量流式请求发送开始: url={url}");
            let result =
                stream::stream_gemini_incremental(self.http.client(), &url, &headers, &body).await;
            match &result {
                Ok(_) => tracing::info!("Gemini 增量流式连接建立成功: url={url}"),
                Err(error) => {
                    tracing::error!("Gemini 增量流式连接失败: url={url}, error={error}");
                }
            }
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessage, ChatRequest};
    use crate::types::ToolDefinition;

    fn make_client() -> GeminiClient {
        GeminiClient::new(
            "https://generativelanguage.googleapis.com/v1beta".into(),
            "test-key".into(),
            "gemini-2.5-flash".into(),
            4096,
            0.7,
            None,
        )
    }

    #[test]
    fn generate_url_construction() {
        let client = make_client();
        assert_eq!(
            client.generate_url(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
    }

    #[test]
    fn stream_url_construction() {
        let client = make_client();
        assert!(client
            .stream_url()
            .ends_with("models/gemini-2.5-flash:streamGenerateContent?alt=sse"));
    }

    #[test]
    fn auth_headers_use_gemini_api_key() {
        let headers = make_client().build_headers();
        assert!(headers
            .iter()
            .any(|(name, value)| *name == "x-goog-api-key" && value == "test-key"));
        assert!(headers
            .iter()
            .any(|(name, value)| *name == "Content-Type" && value == "application/json"));
    }

    #[test]
    fn try_new_rejects_invalid_proxy_without_panicking() {
        let result = GeminiClient::try_new(
            "https://generativelanguage.googleapis.com/v1beta".into(),
            "test-key".into(),
            "gemini-2.5-flash".into(),
            4096,
            0.7,
            Some("not-a-url".into()),
        );
        assert!(result.is_err());
    }

    #[test]
    fn to_request_plain_text_user_message() {
        let client = make_client();
        let request = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("你好")],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };

        let body = client.to_gemini_request(request);
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "你好");
        assert!(body.get("systemInstruction").is_none());
    }

    #[test]
    fn to_request_merges_multiple_system_messages() {
        let client = make_client();
        let request = ChatRequest {
            model: None,
            messages: vec![
                ChatMessage::system("你是助手"),
                ChatMessage::system("用中文回答"),
                ChatMessage::user("你好"),
            ],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };

        let body = client.to_gemini_request(request);
        let merged = body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(merged.contains("你是助手"));
        assert!(merged.contains("用中文回答"));
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
    }

    #[test]
    fn to_request_generation_config_includes_params() {
        let client = make_client();
        let request = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("hi")],
            max_tokens: Some(1024),
            temperature: Some(0.3),
            tools: None,
            tool_choice: None,
        };

        let body = client.to_gemini_request(request);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 1024);
        assert!((body["generationConfig"]["temperature"].as_f64().unwrap() - 0.3).abs() < 1e-9);
    }

    #[test]
    fn to_request_assistant_tool_use_becomes_function_call() {
        let client = make_client();
        let request = ChatRequest {
            model: None,
            messages: vec![
                ChatMessage::user("查天气"),
                ChatMessage::assistant_blocks(vec![
                    ContentBlock::Text {
                        text: "好的".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".into(),
                        name: "get_weather".into(),
                        input: serde_json::json!({"city": "北京"}),
                    },
                ]),
            ],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };

        let body = client.to_gemini_request(request);
        let model_message = &body["contents"][1];
        assert_eq!(model_message["role"], "model");
        assert_eq!(model_message["parts"][0]["text"], "好的");
        assert_eq!(
            model_message["parts"][1]["functionCall"]["name"],
            "get_weather"
        );
        assert_eq!(
            model_message["parts"][1]["functionCall"]["args"]["city"],
            "北京"
        );
    }

    #[test]
    fn to_request_tool_result_becomes_function_response() {
        let client = make_client();
        let request = ChatRequest {
            model: None,
            messages: vec![
                ChatMessage::assistant_blocks(vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "get_weather".into(),
                    input: serde_json::json!({"city": "北京"}),
                }]),
                ChatMessage::tool_result("call_1", "晴，25度", false),
            ],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };

        let body = client.to_gemini_request(request);
        let response_message = &body["contents"][1];
        assert_eq!(response_message["role"], "user");
        assert_eq!(
            response_message["parts"][0]["functionResponse"]["name"],
            "get_weather"
        );
        assert_eq!(
            response_message["parts"][0]["functionResponse"]["response"]["output"],
            "晴，25度"
        );
    }

    #[test]
    fn to_request_preserves_thought_signature_on_function_call() {
        let id = gemini_tool_call_id(1, Some("opaque-signature+/="));
        let request = ChatRequest {
            model: None,
            messages: vec![ChatMessage::assistant_blocks(vec![ContentBlock::ToolUse {
                id,
                name: "get_weather".into(),
                input: json!({"city": "北京"}),
            }])],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        let body = make_client().to_gemini_request(request);
        assert_eq!(
            body["contents"][0]["parts"][0]["thoughtSignature"],
            "opaque-signature+/="
        );
    }

    #[test]
    fn to_request_tools_declared_as_function_declarations() {
        let client = make_client();
        let request = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("hi")],
            max_tokens: None,
            temperature: None,
            tools: Some(vec![ToolDefinition {
                name: "get_weather".into(),
                description: "查询天气".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                }),
            }]),
            tool_choice: None,
        };

        let body = client.to_gemini_request(request);
        let declaration = &body["tools"][0]["functionDeclarations"][0];
        assert_eq!(declaration["name"], "get_weather");
        assert_eq!(declaration["description"], "查询天气");
        assert!(declaration["parametersJsonSchema"]
            .get("properties")
            .is_some());
        assert!(declaration.get("parameters").is_none());
    }

    #[test]
    fn to_request_preserves_full_json_schema_features() {
        let request = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("hi")],
            max_tokens: None,
            temperature: None,
            tools: Some(vec![ToolDefinition {
                name: "config".into(),
                description: "set config".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "value": {"type": ["string", "boolean", "number"]}
                    },
                    "additionalProperties": false
                }),
            }]),
            tool_choice: None,
        };
        let body = make_client().to_gemini_request(request);
        let schema = &body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"];
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["value"]["type"][0], "string");
    }

    #[test]
    fn to_request_tool_choice_modes_are_mapped() {
        for (choice, expected) in [
            (ToolChoice::Auto, "AUTO"),
            (ToolChoice::None, "NONE"),
            (
                ToolChoice::Tool {
                    name: "get_weather".into(),
                },
                "ANY",
            ),
        ] {
            let request = ChatRequest {
                model: None,
                messages: vec![ChatMessage::user("hi")],
                max_tokens: None,
                temperature: None,
                tools: None,
                tool_choice: Some(choice),
            };
            let body = make_client().to_gemini_request(request);
            assert_eq!(
                body["toolConfig"]["functionCallingConfig"]["mode"],
                expected
            );
        }
    }

    #[test]
    fn named_tool_choice_restricts_allowed_function() {
        let request = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("hi")],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: Some(ToolChoice::Tool {
                name: "get_weather".into(),
            }),
        };
        let body = make_client().to_gemini_request(request);
        assert_eq!(
            body["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
            "get_weather"
        );
    }

    #[test]
    fn parse_response_text_only() {
        let response = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "你好世界"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "totalTokenCount": 15
            }
        });
        let parsed = GeminiClient::parse_gemini_response(&response, "gemini-2.5-flash".into());
        assert_eq!(parsed.text(), "你好世界");
        assert_eq!(parsed.usage.prompt_tokens, 10);
        assert_eq!(parsed.usage.completion_tokens, 5);
        assert_eq!(parsed.finish_reason, Some(FinishReason::EndTurn));
    }

    #[test]
    fn parse_response_thinking_part() {
        let response = json!({
            "candidates": [{
                "content": {"parts": [
                    {"text": "让我想想", "thought": true},
                    {"text": "答案是42"}
                ]},
                "finishReason": "STOP"
            }]
        });
        let parsed = GeminiClient::parse_gemini_response(&response, "m".into());
        assert!(matches!(
            &parsed.content[0],
            ContentBlock::Thinking { content } if content == "让我想想"
        ));
        assert_eq!(parsed.text(), "答案是42");
    }

    #[test]
    fn parse_response_function_call_generates_id() {
        let response = json!({
            "candidates": [{
                "content": {"parts": [{
                    "functionCall": {"name": "get_weather", "args": {"city": "北京"}}
                }]},
                "finishReason": "STOP"
            }]
        });
        let parsed = GeminiClient::parse_gemini_response(&response, "m".into());
        assert!(matches!(
            &parsed.content[0],
            ContentBlock::ToolUse { id, name, input }
                if id == "call_1" && name == "get_weather" && input["city"] == "北京"
        ));
        assert_eq!(parsed.finish_reason, Some(FinishReason::ToolUse));
    }

    #[test]
    fn parse_response_function_call_encodes_thought_signature() {
        let response = json!({
            "candidates": [{
                "content": {"parts": [{
                    "functionCall": {"name": "get_weather", "args": {"city": "北京"}},
                    "thoughtSignature": "opaque-signature+/="
                }]},
                "finishReason": "STOP"
            }]
        });
        let parsed = GeminiClient::parse_gemini_response(&response, "m".into());
        let ContentBlock::ToolUse { id, .. } = &parsed.content[0] else {
            panic!("expected tool use");
        };
        assert_eq!(
            thought_signature_from_tool_call_id(id).as_deref(),
            Some("opaque-signature+/=")
        );
    }

    #[test]
    fn parse_response_max_tokens_finish_reason() {
        let response = json!({
            "candidates": [{
                "content": {"parts": [{"text": "截断"}]},
                "finishReason": "MAX_TOKENS"
            }]
        });
        let parsed = GeminiClient::parse_gemini_response(&response, "m".into());
        assert_eq!(parsed.finish_reason, Some(FinishReason::MaxTokens));
    }

    #[test]
    fn parse_response_empty_candidates() {
        let response = json!({"candidates": []});
        let parsed = GeminiClient::parse_gemini_response(&response, "m".into());
        assert!(parsed.content.is_empty());
        assert!(parsed.finish_reason.is_none());
    }
}
