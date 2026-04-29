use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::error::{LlmError, Result};
use crate::provider::{ChatMessage, ChatRequest, ChatResponse, LlmProvider, MessageRole};
use crate::types::{ContentBlock, FinishReason, TokenUsage, ToolChoice};

// ---------------------------------------------------------------------------
// OpenAI-compatible API Client
// ---------------------------------------------------------------------------

pub struct OpenAiCompatClient {
    api_base: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f64,
    client: reqwest::Client,
}

// ---------------------------------------------------------------------------
// API Request / Response Types (OpenAI wire format)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ApiChatRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
}

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
struct ApiMessage {
    #[allow(dead_code)]
    role: String,
    content: Option<serde_json::Value>,
    /// DeepSeek v4 思考模式返回的顶层字段，必须原样传回
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ApiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ApiToolCall {
    id: String,
    #[allow(dead_code)]
    r#type: String,
    function: ApiFunction,
}

#[derive(Debug, Deserialize)]
struct ApiFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

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

    fn chat_url(&self) -> String {
        let base = self.api_base.trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    /// Convert our ChatMessage into OpenAI-compatible JSON values.
    fn message_to_api(msg: &ChatMessage) -> Vec<serde_json::Value> {
        let role = match msg.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };

        let has_tool_use = msg.content.iter().any(ContentBlock::is_tool_use);
        let has_tool_result = msg.content.iter().any(ContentBlock::is_tool_result);

        if has_tool_use {
            let mut text_parts = Vec::new();
            let mut tool_calls = Vec::new();
            let mut reasoning_content: Option<String> = None;

            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        text_parts.push(serde_json::json!({
                            "type": "text",
                            "text": text
                        }));
                    }
                    ContentBlock::Thinking { content } => {
                        // DeepSeek v4: reasoning_content 作为顶层字段
                        reasoning_content = Some(content.clone());
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_calls.push(serde_json::json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": input.to_string()
                            }
                        }));
                    }
                    ContentBlock::ToolResult { .. } => {}
                }
            }

            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), serde_json::json!("assistant"));
            if !text_parts.is_empty() {
                obj.insert("content".into(), serde_json::json!(text_parts));
            }
            if !tool_calls.is_empty() {
                obj.insert("tool_calls".into(), serde_json::json!(tool_calls));
            }
            if let Some(rc) = reasoning_content {
                obj.insert("reasoning_content".into(), serde_json::json!(rc));
            }
            vec![serde_json::Value::Object(obj)]
        } else if has_tool_result {
            msg.content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => Some(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": content
                    })),
                    _ => None,
                })
                .collect()
        } else if msg.role == MessageRole::Assistant && msg.content.iter().any(ContentBlock::is_thinking) {
            // assistant 消息含 thinking — reasoning_content 作为顶层字段
            let text = msg.text_content();
            let thinking = msg.content.iter().find_map(|b| match b {
                ContentBlock::Thinking { content } => Some(content.clone()),
                _ => None,
            });
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), serde_json::json!("assistant"));
            if !text.is_empty() {
                obj.insert("content".into(), serde_json::json!(text));
            }
            if let Some(rc) = thinking {
                obj.insert("reasoning_content".into(), serde_json::json!(rc));
            }
            vec![serde_json::Value::Object(obj)]
        } else {
            let text: String = msg.text_content();
            vec![serde_json::json!({
                "role": role,
                "content": text
            })]
        }
    }

    fn to_api_request(&self, request: ChatRequest) -> ApiChatRequest {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .flat_map(Self::message_to_api)
            .collect();

        let tools = request.tools.map(|tools| {
            serde_json::json!(tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema
                        }
                    })
                })
                .collect::<Vec<_>>())
        });

        let tool_choice = request.tool_choice.map(|tc| match tc {
            ToolChoice::Auto => serde_json::json!("auto"),
            ToolChoice::None => serde_json::json!("none"),
            ToolChoice::Tool { name } => serde_json::json!({
                "type": "function",
                "function": { "name": name }
            }),
        });

        ApiChatRequest {
            model: request.model.unwrap_or_else(|| self.model.clone()),
            messages,
            max_tokens: request.max_tokens.or(Some(self.max_tokens)),
            temperature: request.temperature.or(Some(self.temperature)),
            stream: Some(false),
            tools,
            tool_choice,
        }
    }

    /// Build a streaming API request body (stream=true).
    fn to_stream_api_body(&self, request: ChatRequest) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .flat_map(Self::message_to_api)
            .collect();

        let mut body = serde_json::json!({
            "model": request.model.unwrap_or_else(|| self.model.clone()),
            "messages": messages,
            "stream": true,
        });

        if let Some(max_tokens) = request.max_tokens.or(Some(self.max_tokens)) {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temp) = request.temperature.or(Some(self.temperature)) {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(tools) = request.tools {
            body["tools"] = serde_json::json!(tools
                .iter()
                .map(|t| serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema
                    }
                }))
                .collect::<Vec<_>>());
        }
        if let Some(tc) = request.tool_choice {
            body["tool_choice"] = match tc {
                ToolChoice::Auto => serde_json::json!("auto"),
                ToolChoice::None => serde_json::json!("none"),
                ToolChoice::Tool { name } => serde_json::json!({
                    "type": "function",
                    "function": { "name": name }
                }),
            };
        }

        body
    }

    fn parse_response(api_resp: ApiChatResponse, fallback_model: String) -> ChatResponse {
        let choice = api_resp.choices.first();
        let (content, finish_reason) = match choice {
            Some(ch) => {
                let blocks = if let Some(msg) = &ch.message {
                    Self::parse_api_message(msg)
                } else {
                    vec![]
                };
                let reason = ch.finish_reason.as_deref().map(FinishReason::from_api_str);
                (blocks, reason)
            }
            None => (vec![], None),
        };

        let usage = api_resp
            .usage
            .map_or(TokenUsage::default(), |u| TokenUsage {
                prompt_tokens: u.prompt_tokens.unwrap_or(0),
                completion_tokens: u.completion_tokens.unwrap_or(0),
                total_tokens: u.total_tokens.unwrap_or(0),
            });

        ChatResponse {
            content,
            model: api_resp.model.unwrap_or(fallback_model),
            usage,
            finish_reason,
        }
    }

    /// Parse an API message into content blocks.
    fn parse_api_message(msg: &ApiMessage) -> Vec<ContentBlock> {
        let mut blocks = Vec::new();

        // DeepSeek v4: 顶层 reasoning_content 字段
        if let Some(reasoning) = &msg.reasoning_content {
            if !reasoning.is_empty() {
                blocks.push(ContentBlock::thinking(reasoning));
            }
        }

        if let Some(content) = &msg.content {
            match content {
                serde_json::Value::String(s) => {
                    if !s.is_empty() {
                        let (thinking, text) = extract_thinking_and_text(s);
                        if let Some(t) = thinking {
                            blocks.push(ContentBlock::thinking(t));
                        }
                        if !text.is_empty() {
                            blocks.push(ContentBlock::text(text));
                        }
                    }
                }
                serde_json::Value::Array(parts) => {
                    for part in parts {
                        if let Some(reasoning) =
                            part.get("reasoning_content").and_then(|r| r.as_str())
                        {
                            if !reasoning.is_empty() {
                                blocks.push(ContentBlock::thinking(reasoning));
                            }
                        }
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                blocks.push(ContentBlock::text(text));
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(tool_calls) = &msg.tool_calls {
            for tc in tool_calls {
                let input =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);
                blocks.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    input,
                });
            }
        }

        if blocks.is_empty() {
            blocks.push(ContentBlock::text(""));
        }

        blocks
    }
}

// ---------------------------------------------------------------------------
// Thinking extraction
// ---------------------------------------------------------------------------

/// Extract thinking content and remaining text from a raw LLM response string.
///
/// Supports: `<thinking>...</thinking>`, `<think...>...</think >`, unclosed variants.
fn extract_thinking_and_text(text: &str) -> (Option<String>, String) {
    let mut thinking_parts = Vec::new();
    let mut remaining = text.to_string();

    let re = regex::Regex::new(r"(?s)<think(?:ing)?[^>]*>(.*?)</think(?:ing)?\s*>").unwrap();

    loop {
        if let Some(caps) = re.captures(&remaining) {
            let thinking_content = caps[1].trim();
            if !thinking_content.is_empty() {
                thinking_parts.push(thinking_content.to_string());
            }
            remaining = remaining.replace(&caps[0], "");
        } else {
            break;
        }
    }

    // Handle unclosed <think...> at end of text
    if let Some(start) = remaining.find("<think") {
        let after_start = &remaining[start..];
        if !after_start.contains("</think") {
            let thinking_content = remaining[start..]
                .trim_start_matches(|c: char| c == '<' || c.is_alphabetic() || c == '>')
                .trim();
            if !thinking_content.is_empty() {
                thinking_parts.push(thinking_content.to_string());
            }
            remaining.truncate(start);
        }
    }

    let thinking = if thinking_parts.is_empty() {
        None
    } else {
        Some(thinking_parts.join("\n"))
    };

    (thinking, remaining.trim().to_string())
}

// ---------------------------------------------------------------------------
// LlmProvider impl
// ---------------------------------------------------------------------------

impl LlmProvider for OpenAiCompatClient {
    fn model(&self) -> &str {
        &self.model
    }

    fn complete(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>> {
        let api_request = self.to_api_request(request);
        let url = self.chat_url();
        let api_key = self.api_key.clone();
        let fallback_model = self.model.clone();

        Box::pin(async move {
            let response = self
                .client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&api_request)
                .send()
                .await
                .map_err(|e| LlmError::RequestFailed(format!("HTTP request failed: {e}")))?;

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
                .map_err(|e| LlmError::RequestFailed(format!("Failed to parse response: {e}")))?;

            Ok(Self::parse_response(api_resp, fallback_model))
        })
    }

    fn stream_complete(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<crate::types::StreamEvent>>> + Send + '_>> {
        let body = self.to_stream_api_body(request);
        let url = self.chat_url();
        let api_key = self.api_key.clone();

        Box::pin(
            async move { crate::stream::stream_openai(&self.client, &url, &api_key, &body).await },
        )
    }

    fn stream_incremental(
        &self,
        request: ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = crate::Result<tokio::sync::mpsc::Receiver<crate::types::StreamEvent>>,
                > + Send
                + '_,
        >,
    > {
        let body = self.to_stream_api_body(request);
        let url = self.chat_url();
        let api_key = self.api_key.clone();

        Box::pin(async move {
            crate::stream::stream_openai_incremental(&self.client, &url, &api_key, &body).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            client.chat_url(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn message_to_api_plain_text() {
        let msg = ChatMessage::user("hello");
        let api_msgs = OpenAiCompatClient::message_to_api(&msg);
        assert_eq!(api_msgs.len(), 1);
        assert_eq!(api_msgs[0]["role"], "user");
        assert_eq!(api_msgs[0]["content"], "hello");
    }

    #[test]
    fn message_to_api_tool_use() {
        let msg = ChatMessage::assistant_blocks(vec![
            ContentBlock::text("Let me read that file"),
            ContentBlock::ToolUse {
                id: "call_001".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "/tmp/test.rs"}),
            },
        ]);
        let api_msgs = OpenAiCompatClient::message_to_api(&msg);
        assert_eq!(api_msgs.len(), 1);
        assert_eq!(api_msgs[0]["role"], "assistant");
        assert!(api_msgs[0].get("tool_calls").is_some());
    }

    #[test]
    fn message_to_api_tool_result() {
        let msg = ChatMessage::tool_result("call_001", "file contents", false);
        let api_msgs = OpenAiCompatClient::message_to_api(&msg);
        assert_eq!(api_msgs.len(), 1);
        assert_eq!(api_msgs[0]["role"], "tool");
        assert_eq!(api_msgs[0]["tool_call_id"], "call_001");
    }

    #[test]
    fn parse_api_response_text() {
        let json = r#"{
            "choices": [{"message": {"role": "assistant", "content": "Hello!"}, "finish_reason": "stop"}],
            "model": "glm-4.7",
            "usage": {"prompt_tokens": 10, "completion_tokens": 8, "total_tokens": 18}
        }"#;
        let api_resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let resp = OpenAiCompatClient::parse_response(api_resp, "fallback".into());
        assert_eq!(resp.text(), "Hello!");
        assert_eq!(resp.model, "glm-4.7");
        assert!(!resp.has_tool_calls());
        assert_eq!(resp.finish_reason, Some(FinishReason::EndTurn));
        assert_eq!(resp.usage.total_tokens, 18);
    }

    #[test]
    fn parse_api_response_tool_calls() {
        let json = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{"id": "call_abc", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"/tmp/test.rs\"}"}}]
                },
                "finish_reason": "tool_calls"
            }],
            "model": "glm-5.1",
            "usage": {"prompt_tokens": 50, "completion_tokens": 20, "total_tokens": 70}
        }"#;
        let api_resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let resp = OpenAiCompatClient::parse_response(api_resp, "fallback".into());
        assert!(resp.has_tool_calls());
        assert_eq!(resp.finish_reason, Some(FinishReason::ToolUse));
    }

    #[test]
    fn parse_empty_choices() {
        let json = r#"{"choices": [], "model": "test"}"#;
        let api_resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let resp = OpenAiCompatClient::parse_response(api_resp, "test".into());
        assert_eq!(resp.text(), "");
    }

    // --- extract_thinking_and_text tests ---

    #[test]
    fn thinking_extraction_minimax_format() {
        let input = "<thinking>Let me analyze this...</thinking>\nThe answer is 42.";
        let (thinking, text) = extract_thinking_and_text(input);
        assert_eq!(thinking.as_deref(), Some("Let me analyze this..."));
        assert_eq!(text, "The answer is 42.");
    }

    #[test]
    fn thinking_extraction_deepseek_format() {
        let input = "<think >\nStep 1: ...</think >\nFinal answer here";
        let (thinking, text) = extract_thinking_and_text(input);
        assert!(thinking.is_some());
        assert_eq!(text, "Final answer here");
    }

    #[test]
    fn thinking_extraction_no_tags() {
        let input = "Just a plain response without any thinking.";
        let (thinking, text) = extract_thinking_and_text(input);
        assert!(thinking.is_none());
        assert_eq!(text, "Just a plain response without any thinking.");
    }

    #[test]
    fn thinking_extraction_multiple_blocks() {
        let input = "<thinking>Part 1</thinking> middle <thinking>Part 2</thinking> end";
        let (thinking, text) = extract_thinking_and_text(input);
        assert_eq!(thinking.as_deref(), Some("Part 1\nPart 2"));
        assert_eq!(text, "middle  end");
    }

    #[test]
    fn thinking_extraction_unclosed_tag() {
        let input = "<thinking\nSome reasoning here";
        let (thinking, text) = extract_thinking_and_text(input);
        assert!(thinking.is_some());
        assert_eq!(text, "");
    }

    #[test]
    fn parse_api_message_with_thinking() {
        let json = r#"{
            "choices": [{
                "message": {"role": "assistant", "content": "<thinking>internal reasoning</thinking>The actual answer"},
                "finish_reason": "stop"
            }],
            "model": "test"
        }"#;
        let api_resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let resp = OpenAiCompatClient::parse_response(api_resp, "fallback".into());
        assert_eq!(resp.text(), "The actual answer");
        assert!(resp.content.iter().any(|b| b.is_thinking()));
    }

    #[test]
    fn parse_api_message_with_reasoning_content_field() {
        let json = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [{"reasoning_content": "step by step"}, {"text": "The answer"}]
                },
                "finish_reason": "stop"
            }],
            "model": "test"
        }"#;
        let api_resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let resp = OpenAiCompatClient::parse_response(api_resp, "fallback".into());
        assert_eq!(resp.text(), "The answer");
        assert!(resp.content.iter().any(|b| b.is_thinking()));
    }
}
