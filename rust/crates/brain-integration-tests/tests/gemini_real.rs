//! Ignored end-to-end tests for the native Gemini provider.
//!
//! Run with:
//! `cargo test -p brain-integration-tests --test gemini_real -- --ignored --nocapture`

use brain_llm::{
    ChatMessage, ChatRequest, ContentBlock, GeminiClient, LlmConfig, LlmProvider, ProviderKind,
    StreamEvent, ToolChoice, ToolDefinition,
};

fn real_gemini_client(model: &str) -> GeminiClient {
    if let Ok(config) = LlmConfig::load_default() {
        if let Some((provider_name, provider)) = config
            .llm
            .providers
            .iter()
            .find(|(_, provider)| provider.kind == ProviderKind::Gemini)
        {
            let key = config
                .resolve_api_key(provider_name)
                .expect("Gemini provider exists but its API key cannot be resolved");
            let proxy = config
                .resolve_proxy(provider_name)
                .expect("Gemini provider proxy configuration is invalid");
            return GeminiClient::try_new(
                provider.api_base.clone(),
                key,
                model.into(),
                8192,
                0.7,
                proxy,
            )
            .expect("Gemini client construction failed");
        }
    }

    let key = std::env::var("GEMINI_API_KEY").expect(
        "configure a kind=\"gemini\" provider in ~/.ai-brain/config.toml or set GEMINI_API_KEY",
    );
    GeminiClient::try_new(
        "https://generativelanguage.googleapis.com/v1beta".into(),
        key,
        model.into(),
        8192,
        0.7,
        std::env::var("HTTPS_PROXY").ok(),
    )
    .expect("Gemini client construction failed")
}

fn request(prompt: &str) -> ChatRequest {
    ChatRequest {
        model: None,
        messages: vec![ChatMessage::user(prompt)],
        max_tokens: Some(512),
        temperature: Some(0.3),
        tools: None,
        tool_choice: None,
    }
}

#[tokio::test]
#[ignore = "requires GEMINI_API_KEY and network access"]
async fn real_gemini_accepts_full_brain_tool_catalog() {
    let client = real_gemini_client("gemini-2.5-flash");
    let tools = tools::mvp_tool_specs()
        .iter()
        .map(|spec| ToolDefinition {
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            input_schema: spec.input_schema.clone(),
        })
        .collect();
    let response = client
        .complete(ChatRequest {
            tools: Some(tools),
            tool_choice: Some(ToolChoice::Auto),
            ..request("只回复 OK，不要调用工具")
        })
        .await
        .expect("Gemini should accept the complete AI Brain tool catalog");
    assert!(!response.text().is_empty());
}

#[tokio::test]
#[ignore = "requires GEMINI_API_KEY and network access"]
async fn real_gemini_connectivity() {
    let client = real_gemini_client("gemini-2.5-flash");
    let response = client.complete(request("用一句话说你好")).await.unwrap();
    assert!(!response.text().is_empty());
    assert!(response.usage.total_tokens > 0);
}

#[tokio::test]
#[ignore = "requires GEMINI_API_KEY and network access"]
async fn real_gemini_function_calling_round_trip() {
    let client = real_gemini_client("gemini-2.5-flash");
    let tools = vec![ToolDefinition {
        name: "get_weather".into(),
        description: "查询某城市天气".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }),
    }];
    let first = client
        .complete(ChatRequest {
            tools: Some(tools.clone()),
            tool_choice: Some(ToolChoice::Auto),
            ..request("北京今天天气怎么样？请调用 get_weather")
        })
        .await
        .unwrap();
    let (id, name, input) = match first.tool_calls().first() {
        Some(ContentBlock::ToolUse { id, name, input }) => {
            (id.clone(), name.clone(), input.clone())
        }
        other => panic!("expected a tool call, got {other:?}"),
    };
    let second = client
        .complete(ChatRequest {
            model: None,
            messages: vec![
                ChatMessage::user("北京今天天气怎么样？请调用 get_weather"),
                ChatMessage::assistant_blocks(vec![ContentBlock::ToolUse {
                    id: id.clone(),
                    name,
                    input,
                }]),
                ChatMessage::tool_result(id, "晴，25度", false),
            ],
            max_tokens: Some(512),
            temperature: Some(0.3),
            tools: Some(tools),
            tool_choice: Some(ToolChoice::Auto),
        })
        .await
        .unwrap();
    assert!(!second.text().is_empty());
}

#[tokio::test]
#[ignore = "requires GEMINI_API_KEY and network access"]
async fn real_gemini_incremental_streaming() {
    let client = real_gemini_client("gemini-2.5-flash");
    let mut receiver = client
        .stream_incremental(request("写三句话介绍 Rust"))
        .await
        .unwrap();
    let mut text = String::new();
    let mut done = false;
    while let Some(event) = receiver.recv().await {
        match event {
            StreamEvent::TextDelta { text: delta } => text.push_str(&delta),
            StreamEvent::Done { .. } => done = true,
            _ => {}
        }
    }
    assert!(!text.is_empty());
    assert!(done);
}

#[tokio::test]
#[ignore = "requires network access"]
async fn real_gemini_invalid_key_errors() {
    let client = GeminiClient::new(
        "https://generativelanguage.googleapis.com/v1beta".into(),
        "invalid-key".into(),
        "gemini-2.5-flash".into(),
        64,
        0.3,
        None,
    );
    assert!(client.complete(request("hi")).await.is_err());
}

#[tokio::test]
#[ignore = "requires GEMINI_API_KEY and network access"]
async fn real_gemini_nonexistent_model_errors() {
    let client = real_gemini_client("gemini-not-a-real-model");
    assert!(client.complete(request("hi")).await.is_err());
}
