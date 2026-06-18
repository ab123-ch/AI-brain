# Gemini 原生 Provider 适配实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在 `brain-llm` crate 新增 GeminiClient 走原生 Gemini API，并通过 `ProviderConfig.kind` 路由 + 全局/Provider 级代理配置，使智脑可按脑接入 Gemini。

**Architecture:** 独立 `GeminiClient` 实现 `LlmProvider` trait（方案 A）。新增 `SharedHttpClient` 封装 reqwest+代理+重试，供 GeminiClient 复用。所有上层 crate 零改动，`LlmProvider` 契约不变。详见 `docs/plans/2026-06-18-gemini-provider-design.md`。

**Tech Stack:** Rust, reqwest 0.12, serde, tokio, 现有 `brain-llm` crate。

**验证命令（每个任务结束运行）:**
- 单 crate: `cargo test -p brain-llm`
- 全量: `cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

**TDD 约定（Rust 适配）:** Rust 是编译型语言，"先写测试看它失败"表现为编译失败或测试断言失败。每个任务的 Step 2 即为该验证点。

---

## Task 1: 配置类型扩展 + resolve_proxy

为 `ProviderConfig` 增加 `kind`（协议路由）和 `proxy`（代理设置）字段，新增全局 `ProxySection`，实现 `resolve_proxy` 三态解析。

**Files:**
- Modify: `rust/crates/brain-llm/src/config.rs`
- Test: 同文件 `#[cfg(test)] mod tests`

### Step 1: 写失败测试

在 `config.rs` 的 `mod tests` 末尾追加：

```rust
    #[test]
    fn provider_kind_defaults_to_openai() {
        let toml = r#"
[llm]
default_provider = "x"
default_model = "m"

[llm.providers.x]
api_base = "https://x.example.com/v1"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, toml).unwrap();
        let cfg = LlmConfig::load(&path).unwrap();
        let pc = cfg.llm.providers.get("x").unwrap();
        assert_eq!(pc.kind, ProviderKind::OpenAi);
        assert!(pc.proxy.is_none());
    }

    #[test]
    fn provider_kind_gemini_parsed() {
        let toml = r#"
[llm]
default_provider = "g"
default_model = "gemini-2.5-flash"

[llm.providers.g]
api_base = "https://generativelanguage.googleapis.com/v1beta"
api_key_env = "GEMINI_API_KEY"
kind = "gemini"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, toml).unwrap();
        let cfg = LlmConfig::load(&path).unwrap();
        let pc = cfg.llm.providers.get("g").unwrap();
        assert_eq!(pc.kind, ProviderKind::Gemini);
    }

    #[test]
    fn resolve_proxy_follows_global_default() {
        let mut cfg = LlmConfig::default_config();
        cfg.proxy.default = Some("http://127.0.0.1:7890".into());
        assert_eq!(
            cfg.resolve_proxy("xiaomi").unwrap(),
            Some("http://127.0.0.1:7890".to_string())
        );
    }

    #[test]
    fn resolve_proxy_none_disables() {
        let mut cfg = LlmConfig::default_config();
        cfg.proxy.default = Some("http://127.0.0.1:7890".into());
        if let Some(p) = cfg.llm.providers.get_mut("xiaomi") {
            p.proxy = Some("none".into());
        }
        assert_eq!(cfg.resolve_proxy("xiaomi").unwrap(), None);
    }

    #[test]
    fn resolve_proxy_custom_overrides_global() {
        let mut cfg = LlmConfig::default_config();
        cfg.proxy.default = Some("http://127.0.0.1:7890".into());
        if let Some(p) = cfg.llm.providers.get_mut("xiaomi") {
            p.proxy = Some("http://127.0.0.1:1080".into());
        }
        assert_eq!(
            cfg.resolve_proxy("xiaomi").unwrap(),
            Some("http://127.0.0.1:1080".to_string())
        );
    }

    #[test]
    fn resolve_proxy_no_global_returns_none() {
        let cfg = LlmConfig::default_config();
        // default_config 的 proxy.default 是 None
        assert_eq!(cfg.resolve_proxy("xiaomi").unwrap(), None);
    }
```

### Step 2: 跑测试验证失败

Run: `cargo test -p brain-llm config::tests 2>&1 | tail -20`
Expected: 编译失败（`ProviderKind` 未定义、`pc.kind`/`pc.proxy`/`cfg.proxy`/`resolve_proxy` 不存在）

### Step 3: 实现

在 `config.rs` 顶部 `use` 之后加入枚举：

```rust
/// Provider 协议类型，决定路由到哪个 Client
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    OpenAi,
    Gemini,
}
```

修改 `ProviderConfig`，新增 `kind` 和 `proxy` 字段：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_base: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub api_key: Option<String>,
    /// 协议类型，缺省 openai（向后兼容旧配置）
    #[serde(default)]
    pub kind: ProviderKind,
    /// 代理设置：None=跟随全局，Some("none")=关闭，Some(url)=独立地址
    #[serde(default)]
    pub proxy: Option<String>,
}
```

新增 `ProxySection`，并给 `LlmConfig` 加 `proxy` 字段：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxySection {
    /// 全局默认代理地址，不配则全程不走代理
    #[serde(default)]
    pub default: Option<String>,
}
```

在 `LlmConfig` 结构体加入（注意字段顺序与 `default_config()` 一致）：

```rust
pub struct LlmConfig {
    pub llm: LlmSection,
    #[serde(default)]
    pub brain: BrainSection,
    /// 全局代理配置
    #[serde(default)]
    pub proxy: ProxySection,
    #[serde(default)]
    pub hooks: Option<toml::Value>,
}
```

在 `impl LlmConfig` 中新增 `resolve_proxy` 方法（放在 `resolve_api_key` 之后）：

```rust
    /// 解析某 provider 最终使用的代理地址
    ///
    /// 优先级：provider.proxy（"none"=关闭 / url=独立） > proxy.default（全局）
    pub fn resolve_proxy(&self, provider_name: &str) -> Result<Option<String>> {
        let provider = self
            .llm
            .providers
            .get(provider_name)
            .ok_or_else(|| LlmError::ProviderNotFound(provider_name.to_string()))?;

        Ok(match &provider.proxy {
            Some(s) if s == "none" => None,
            Some(s) => Some(s.clone()),
            None => self.proxy.default.clone(),
        })
    }
```

更新 `default_config()` 的返回值，加入 `proxy: ProxySection::default()`：

```rust
        Self {
            llm: LlmSection { /* 不变 */ },
            brain: BrainSection::default(),
            proxy: ProxySection::default(),
            hooks: None,
        }
```

### Step 4: 跑测试验证通过

Run: `cargo test -p brain-llm config::tests 2>&1 | tail -20`
Expected: PASS（含 6 个新测试 + 既有测试全过）

### Step 5: 提交

```bash
git add rust/crates/brain-llm/src/config.rs
git commit -m "feat(brain-llm): ProviderConfig 加 kind/proxy 字段 + resolve_proxy 三态解析"
```

---

## Task 2: SharedHttpClient（共享 HTTP + 代理 + 重试）

新增 `http_client.rs`，封装 reqwest::Client 构造（含代理注入）与重试配置，供 GeminiClient 复用。

**Files:**
- Create: `rust/crates/brain-llm/src/http_client.rs`
- Modify: `rust/crates/brain-llm/src/lib.rs`（加 `pub mod http_client;`）
- Test: `http_client.rs` 内 `#[cfg(test)]`

### Step 1: 写失败测试

在新建的 `http_client.rs` 末尾写入：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_without_proxy_succeeds() {
        let client = SharedHttpClient::new(None, RetryConfig::default());
        assert!(client.is_ok());
    }

    #[test]
    fn build_with_http_proxy_succeeds() {
        let client = SharedHttpClient::new(Some("http://127.0.0.1:7890"), RetryConfig::default());
        assert!(client.is_ok());
    }

    #[test]
    fn build_with_invalid_proxy_fails() {
        let client = SharedHttpClient::new(Some("not-a-url"), RetryConfig::default());
        assert!(client.is_err());
    }

    #[test]
    fn retry_config_preserved() {
        let retry = RetryConfig {
            max_retries: 5,
            ..RetryConfig::default()
        };
        let client = SharedHttpClient::new(None, retry).unwrap();
        assert_eq!(client.retry_config().max_retries, 5);
    }
}
```

### Step 2: 跑测试验证失败

Run: `cargo test -p brain-llm http_client::tests 2>&1 | tail -20`
Expected: 编译失败（`SharedHttpClient` 未定义）

### Step 3: 实现

`http_client.rs` 完整内容：

```rust
//! 共享 HTTP 客户端：封装 reqwest::Client 构造（含代理注入）与重试配置。
//!
//! 供 GeminiClient 及未来迁移后的 OpenAiCompatClient 复用，
//! 统一代理、超时、重试策略。

use std::time::Duration;

use crate::error::{LlmError, Result};
use crate::openai_compat::RetryConfig;

/// 共享 HTTP 客户端
pub struct SharedHttpClient {
    client: reqwest::Client,
    retry_config: RetryConfig,
}

impl SharedHttpClient {
    /// 构造客户端。`proxy_url` 为 None 时不走代理。
    ///
    /// 仅支持 `http://` 代理（reqwest 未开 socks feature）。
    pub fn new(proxy_url: Option<&str>, retry_config: RetryConfig) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300));

        if let Some(url) = proxy_url {
            let proxy = reqwest::Proxy::all(url)
                .map_err(|e| LlmError::Config(format!("无效的代理地址 '{url}': {e}")))?;
            builder = builder.proxy(proxy);
        }

        let client = builder
            .build()
            .map_err(|e| LlmError::RequestFailed(format!("构造 HTTP 客户端失败: {e}")))?;

        Ok(Self {
            client,
            retry_config,
        })
    }

    /// 只读访问内部 reqwest::Client
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// 只读访问重试配置
    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }
}
```

在 `lib.rs` 加入模块声明（在 `pub mod echo;` 之后）：

```rust
pub mod http_client;
```

### Step 4: 跑测试验证通过

Run: `cargo test -p brain-llm http_client::tests 2>&1 | tail -20`
Expected: PASS（4 个测试）

### Step 5: 提交

```bash
git add rust/crates/brain-llm/src/http_client.rs rust/crates/brain-llm/src/lib.rs
git commit -m "feat(brain-llm): 新增 SharedHttpClient 封装 reqwest+代理+重试"
```

---

## Task 3: GeminiClient 骨架 + 请求转换（文本 + system 合并）

新建 `gemini.rs`，实现 `GeminiClient` 结构体与 `to_gemini_request` 的基础转换（纯文本消息 + 多 system 消息合并为单一 systemInstruction）。

**Files:**
- Create: `rust/crates/brain-llm/src/gemini.rs`
- Modify: `rust/crates/brain-llm/src/lib.rs`（加 `pub mod gemini;`）
- Test: `gemini.rs` 内 `#[cfg(test)]`

### Step 1: 写失败测试

在新建的 `gemini.rs` 末尾写入测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessage, ChatRequest, MessageRole};
    use crate::types::ContentBlock;

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
        let c = make_client();
        assert_eq!(
            c.generate_url(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
    }

    #[test]
    fn stream_url_construction() {
        let c = make_client();
        assert!(c.stream_url().ends_with(
            "models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        ));
    }

    #[test]
    fn to_request_plain_text_user_message() {
        let c = make_client();
        let req = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("你好")],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        let body = c.to_gemini_request(req);
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "你好");
        // 无 system 消息时不应出现 systemInstruction
        assert!(body.get("systemInstruction").is_none());
    }

    #[test]
    fn to_request_merges_multiple_system_messages() {
        let c = make_client();
        let req = ChatRequest {
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
        let body = c.to_gemini_request(req);
        let si = &body["systemInstruction"];
        let merged = si["parts"][0]["text"].as_str().unwrap();
        assert!(merged.contains("你是助手"));
        assert!(merged.contains("用中文回答"));
        // contents 中不应包含 system 角色消息
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
    }

    #[test]
    fn to_request_generation_config_includes_params() {
        let c = make_client();
        let req = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("hi")],
            max_tokens: Some(1024),
            temperature: Some(0.3),
            tools: None,
            tool_choice: None,
        };
        let body = c.to_gemini_request(req);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 1024);
        assert!((body["generationConfig"]["temperature"].as_f64().unwrap() - 0.3).abs() < 1e-9);
    }
}
```

### Step 2: 跑测试验证失败

Run: `cargo test -p brain-llm gemini::tests 2>&1 | tail -20`
Expected: 编译失败（`GeminiClient` 未定义）

### Step 3: 实现

`gemini.rs` 起始内容（本任务只做骨架 + 文本/system 转换，工具相关留 Task 4）：

```rust
//! Gemini 原生 API 客户端 — 走 generateContent / streamGenerateContent。
//!
//! 与 OpenAI 兼容路径并存的独立 Client，实现 LlmProvider trait。
//! 协议差异（systemInstruction 独立字段、functionCall 无 id、thought part 等）
//! 全部在本模块内部消化，上层零感知。

use serde_json::{json, Value};

use crate::error::Result;
use crate::http_client::SharedHttpClient;
use crate::openai_compat::RetryConfig;
use crate::provider::{ChatRequest, MessageRole};
use crate::types::ContentBlock;

pub struct GeminiClient {
    api_base: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f64,
    http: SharedHttpClient,
}

impl GeminiClient {
    pub fn new(
        api_base: String,
        api_key: String,
        model: String,
        max_tokens: u32,
        temperature: f64,
        proxy_url: Option<String>,
    ) -> Self {
        let http = SharedHttpClient::new(
            proxy_url.as_deref(),
            RetryConfig::default(),
        )
        .expect("SharedHttpClient 构造失败");
        Self {
            api_base,
            api_key,
            model,
            max_tokens,
            temperature,
            http,
        }
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

    /// 将 ChatRequest 转换为 Gemini 请求体
    pub(crate) fn to_gemini_request(&self, request: ChatRequest) -> Value {
        // 1. 分离 system 消息（合并）与其余消息
        let mut system_parts: Vec<String> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();

        for msg in &request.messages {
            if msg.role == MessageRole::System {
                let text = msg.text_content();
                if !text.is_empty() {
                    system_parts.push(text);
                }
                continue;
            }
            // 非工具消息的文本转换（工具消息留 Task 4）
            let role = match msg.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "model",
                MessageRole::System => unreachable!(),
                MessageRole::Tool => "user", // tool role 兜底，Task 4 细化
            };
            let text = msg.text_content();
            contents.push(json!({
                "role": role,
                "parts": [{"text": text}]
            }));
        }

        let mut body = json!({
            "contents": contents,
        });

        if !system_parts.is_empty() {
            body["systemInstruction"] = json!({
                "parts": [{"text": system_parts.join("\n\n")}]
            });
        }

        let max_output = request.max_tokens.unwrap_or(self.max_tokens);
        let temp = request.temperature.unwrap_or(self.temperature);
        body["generationConfig"] = json!({
            "maxOutputTokens": max_output,
            "temperature": temp,
        });

        body
    }
}
```

在 `lib.rs` 加模块声明：

```rust
pub mod gemini;
```

### Step 4: 跑测试验证通过

Run: `cargo test -p brain-llm gemini::tests 2>&1 | tail -20`
Expected: PASS（5 个测试）

### Step 5: 提交

```bash
git add rust/crates/brain-llm/src/gemini.rs rust/crates/brain-llm/src/lib.rs
git commit -m "feat(brain-llm): GeminiClient 骨架 + 文本/system 合并转换"
```

---

## Task 4: 请求转换扩展（工具调用 + tool_choice）

扩展 `to_gemini_request`，处理 ToolUse / ToolResult / 工具声明 / tool_choice。

**Files:**
- Modify: `rust/crates/brain-llm/src/gemini.rs`

### Step 1: 写失败测试

在 `gemini.rs` 的 `mod tests` 追加：

```rust
    #[test]
    fn to_request_assistant_tool_use_becomes_function_call() {
        let c = make_client();
        let req = ChatRequest {
            model: None,
            messages: vec![
                ChatMessage::user("查天气"),
                ChatMessage::assistant_blocks(vec![
                    ContentBlock::Text { text: "好的".into() },
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
        let body = c.to_gemini_request(req);
        let contents = body["contents"].as_array().unwrap();
        // assistant 消息应为 model 角色，含 text + functionCall 两个 part
        let model_msg = &contents[1];
        assert_eq!(model_msg["role"], "model");
        let parts = model_msg["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "好的");
        assert_eq!(parts[1]["functionCall"]["name"], "get_weather");
        assert_eq!(parts[1]["functionCall"]["args"]["city"], "北京");
    }

    #[test]
    fn to_request_tool_result_becomes_function_response() {
        let c = make_client();
        let req = ChatRequest {
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
        let body = c.to_gemini_request(req);
        let contents = body["contents"].as_array().unwrap();
        // tool_result 消息应为 user 角色，含 functionResponse part
        let resp_msg = &contents[1];
        assert_eq!(resp_msg["role"], "user");
        let parts = resp_msg["parts"].as_array().unwrap();
        assert_eq!(parts[0]["functionResponse"]["name"], "get_weather");
        assert_eq!(parts[0]["functionResponse"]["response"]["output"], "晴，25度");
    }

    #[test]
    fn to_request_tools_declared_as_function_declarations() {
        let c = make_client();
        let req = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("hi")],
            max_tokens: None,
            temperature: None,
            tools: Some(vec![crate::types::ToolDefinition {
                name: "get_weather".into(),
                description: "查询天气".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}}
                }),
            }]),
            tool_choice: None,
        };
        let body = c.to_gemini_request(req);
        let tools = body["tools"][0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["description"], "查询天气");
        assert!(tools[0]["parameters"].get("properties").is_some());
    }

    #[test]
    fn to_request_tool_choice_auto_maps_to_mode() {
        let c = make_client();
        let req = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("hi")],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: Some(crate::types::ToolChoice::Auto),
        };
        let body = c.to_gemini_request(req);
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
    }

    #[test]
    fn to_request_tool_choice_none_maps_to_mode() {
        let c = make_client();
        let req = ChatRequest {
            model: None,
            messages: vec![ChatMessage::user("hi")],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: Some(crate::types::ToolChoice::None),
        };
        let body = c.to_gemini_request(req);
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "NONE");
    }
```

### Step 2: 跑测试验证失败

Run: `cargo test -p brain-llm gemini::tests 2>&1 | tail -20`
Expected: FAIL（当前实现把 ToolUse/ToolResult 当纯文本，functionCall/functionResponse 不存在）

### Step 3: 实现

重写 `to_gemini_request`（替换 Task 3 的版本），加入工具处理。注意需要在文件顶部 `use` 中补充 `ToolChoice`：

```rust
use crate::types::{ContentBlock, ToolChoice};
```

替换 `to_gemini_request` 方法整体：

```rust
    /// 将 ChatRequest 转换为 Gemini 请求体
    pub(crate) fn to_gemini_request(&self, request: ChatRequest) -> Value {
        let mut system_parts: Vec<String> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();

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
                // user 角色 + functionResponse parts
                msg.content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolResult {
                            content, name, ..
                        } => {
                            // Gemini functionResponse 用 name 关联；name 缺省时用 tool_use_id
                            let resp_name = name.clone().unwrap_or_default();
                            if resp_name.is_empty() {
                                None
                            } else {
                                Some(json!({
                                    "functionResponse": {
                                        "name": resp_name,
                                        "response": {"output": content}
                                    }
                                }))
                            }
                        }
                        // 注意：ToolResult 可能没有 name 字段；见下方处理
                        _ => None,
                    })
                    .collect()
            } else if has_tool_use {
                // model 角色 + text/functionCall parts
                msg.content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(json!({"text": text})),
                        ContentBlock::ToolUse { name, input, .. } => {
                            Some(json!({"functionCall": {"name": name, "args": input}}))
                        }
                        ContentBlock::Thinking { .. } => None, // 请求侧丢弃 thinking
                        _ => None,
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

        let mut body = json!({"contents": contents});

        if !system_parts.is_empty() {
            body["systemInstruction"] = json!({"parts": [{"text": system_parts.join("\n\n")}]});
        }

        // 工具声明
        if let Some(tools) = request.tools.as_ref() {
            if !tools.is_empty() {
                let fds: Vec<Value> = tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema
                        })
                    })
                    .collect();
                body["tools"] = json!([{"functionDeclarations": fds}]);
            }
        }

        // tool_choice 映射
        if let Some(tc) = request.tool_choice {
            let mode = match tc {
                ToolChoice::Auto => "AUTO",
                ToolChoice::None => "NONE",
                ToolChoice::Tool { .. } => "ANY",
            };
            body["toolConfig"] = json!({"functionCallingConfig": {"mode": mode}});
        }

        let max_output = request.max_tokens.unwrap_or(self.max_tokens);
        let temp = request.temperature.unwrap_or(self.temperature);
        body["generationConfig"] = json!({
            "maxOutputTokens": max_output,
            "temperature": temp,
        });

        body
    }
```

**重要：ToolResult 的 name 字段。** 现有 `ContentBlock::ToolResult` 结构需确认是否含 `name`。
查看 `brain-core/src/types.rs` 中 `ContentBlock::ToolResult` 的定义：
- 若字段为 `{ tool_use_id, content, is_error }`（无 name），则需在转换时维护 `tool_use_id → name` 映射。
- 本计划假设需要从历史 ToolUse 消息中查 name。

为此新增一个辅助函数，遍历历史消息建立 `tool_use_id → name` 映射，供 ToolResult 转换使用。
在 `to_gemini_request` 开头加入映射构建，并修改 ToolResult 分支：

```rust
        // 构建 tool_use_id -> name 映射（Gemini functionResponse 用 name 关联）
        let mut id_to_name: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for msg in &request.messages {
            for b in &msg.content {
                if let ContentBlock::ToolUse { id, name, .. } = b {
                    id_to_name.insert(id.as_str(), name.as_str());
                }
            }
        }
```

ToolResult 分支改用映射查 name：

```rust
                        ContentBlock::ToolResult {
                            tool_use_id, content, ..
                        } => {
                            let resp_name = id_to_name.get(tool_use_id.as_str()).copied().unwrap_or("");
                            if resp_name.is_empty() {
                                None
                            } else {
                                Some(json!({
                                    "functionResponse": {
                                        "name": resp_name,
                                        "response": {"output": content}
                                    }
                                }))
                            }
                        }
```

> 注意：Task 3 测试中 `to_request_tool_result_becomes_function_response` 依赖此映射，
> 前一条 assistant 消息含 ToolUse(id="call_1", name="get_weather")，映射会正确命中。

### Step 4: 跑测试验证通过

Run: `cargo test -p brain-llm gemini::tests 2>&1 | tail -20`
Expected: PASS（10 个测试，含本任务 5 个 + Task 3 的 5 个）

### Step 5: 提交

```bash
git add rust/crates/brain-llm/src/gemini.rs
git commit -m "feat(brain-llm): GeminiClient 请求转换支持工具调用/tool_choice"
```

---

## Task 5: 响应解析（text + thinking + functionCall）

实现 `parse_gemini_response`，将 Gemini 响应体转为 `ChatResponse`。处理 functionCall 无 id 的自生成。

**Files:**
- Modify: `rust/crates/brain-llm/src/gemini.rs`

### Step 1: 写失败测试

在 `gemini.rs` 的 `mod tests` 追加：

```rust
    #[test]
    fn parse_response_text_only() {
        let c = make_client();
        let resp = serde_json::json!({
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
        let parsed = GeminiClient::parse_gemini_response(resp, "gemini-2.5-flash".into());
        assert_eq!(parsed.text(), "你好世界");
        assert_eq!(parsed.model, "gemini-2.5-flash");
        assert_eq!(parsed.usage.prompt_tokens, 10);
        assert_eq!(parsed.usage.completion_tokens, 5);
        assert_eq!(parsed.finish_reason, Some(crate::types::FinishReason::EndTurn));
    }

    #[test]
    fn parse_response_thinking_part() {
        let c = make_client();
        let _ = c;
        let resp = serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "让我想想", "thought": true},
                    {"text": "答案是42"}
                ]},
                "finishReason": "STOP"
            }]
        });
        let parsed = GeminiClient::parse_gemini_response(resp, "m".into());
        let thinking: Vec<_> = parsed
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Thinking { content } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking, vec!["让我想想".to_string()]);
        assert_eq!(parsed.text(), "答案是42");
    }

    #[test]
    fn parse_response_function_call_generates_id() {
        let resp = serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"functionCall": {"name": "get_weather", "args": {"city": "北京"}}}
                ]},
                "finishReason": "STOP"
            }]
        });
        let parsed = GeminiClient::parse_gemini_response(resp, "m".into());
        let tool_uses: Vec<_> = parsed
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].1, "get_weather");
        assert_eq!(tool_uses[0].0, "call_1"); // 自生成 id
        assert_eq!(tool_uses[0].2["city"], "北京");
        assert_eq!(parsed.finish_reason, Some(crate::types::FinishReason::ToolUse));
    }

    #[test]
    fn parse_response_max_tokens_finish_reason() {
        let resp = serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "截断"}]},
                "finishReason": "MAX_TOKENS"
            }]
        });
        let parsed = GeminiClient::parse_gemini_response(resp, "m".into());
        assert_eq!(parsed.finish_reason, Some(crate::types::FinishReason::MaxTokens));
    }

    #[test]
    fn parse_response_empty_candidates() {
        let resp = serde_json::json!({"candidates": []});
        let parsed = GeminiClient::parse_gemini_response(resp, "m".into());
        assert!(parsed.content.is_empty());
        assert!(parsed.finish_reason.is_none());
    }
```

### Step 2: 跑测试验证失败

Run: `cargo test -p brain-llm gemini::tests 2>&1 | tail -20`
Expected: 编译失败（`parse_gemini_response` 未定义）

### Step 3: 实现

在 `gemini.rs` 顶部 `use` 补充：

```rust
use crate::provider::ChatResponse;
use crate::types::{FinishReason, TokenUsage};
```

在 `impl GeminiClient` 中新增关联函数：

```rust
    /// 将 Gemini 响应体解析为 ChatResponse
    pub(crate) fn parse_gemini_response(
        body: Value,
        fallback_model: String,
    ) -> ChatResponse {
        let mut content: Vec<ContentBlock> = Vec::new();
        let mut finish_reason: Option<FinishReason> = None;
        let mut call_seq: u32 = 0;

        if let Some(candidates) = body.get("candidates").and_then(|c| c.as_array()) {
            if let Some(first) = candidates.first() {
                if let Some(parts) = first
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                {
                    for part in parts {
                        // thinking part（thought: true）
                        let is_thought = part
                            .get("thought")
                            .and_then(|t| t.as_bool())
                            .unwrap_or(false);
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if is_thought {
                                content.push(ContentBlock::Thinking {
                                    content: text.to_string(),
                                });
                            } else {
                                content.push(ContentBlock::Text {
                                    text: text.to_string(),
                                });
                            }
                        }
                        // functionCall part
                        if let Some(fc) = part.get("functionCall") {
                            if let (Some(name), args) = (
                                fc.get("name").and_then(|n| n.as_str()),
                                fc.get("args").cloned().unwrap_or(serde_json::json!({})),
                            ) {
                                call_seq += 1;
                                content.push(ContentBlock::ToolUse {
                                    id: format!("call_{call_seq}"),
                                    name: name.to_string(),
                                    input: args,
                                });
                            }
                        }
                    }
                }
                // finishReason 映射
                if let Some(reason) = first.get("finishReason").and_then(|r| r.as_str()) {
                    finish_reason = Some(Self::finish_reason_from_str(reason, &content));
                }
            }
        }

        // usageMetadata
        let usage = body
            .get("usageMetadata")
            .map(|u| TokenUsage {
                prompt_tokens: u
                    .get("promptTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                completion_tokens: u
                    .get("candidatesTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                total_tokens: u
                    .get("totalTokenCount")
                    .and_then(|v| v.as_u64())
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

    /// Gemini finishReason 字符串 → FinishReason
    ///
    /// 含 functionCall 时强制 ToolUse（Gemini 的 STOP 也可能伴随工具调用）
    fn finish_reason_from_str(s: &str, content: &[ContentBlock]) -> FinishReason {
        let has_tool_use = content.iter().any(ContentBlock::is_tool_use);
        match s {
            "MAX_TOKENS" => FinishReason::MaxTokens,
            "STOP" if has_tool_use => FinishReason::ToolUse,
            "STOP" => FinishReason::EndTurn,
            // SAFETY/MPI/RECITATION 等都归为 EndTurn（保守）
            _ => {
                if has_tool_use {
                    FinishReason::ToolUse
                } else {
                    FinishReason::EndTurn
                }
            }
        }
    }
```

### Step 4: 跑测试验证通过

Run: `cargo test -p brain-llm gemini::tests 2>&1 | tail -20`
Expected: PASS（15 个测试）

### Step 5: 提交

```bash
git add rust/crates/brain-llm/src/gemini.rs
git commit -m "feat(brain-llm): GeminiClient 响应解析（text/thinking/functionCall）"
```

---

## Task 6: Gemini SSE 流式解析

在 `stream.rs` 新增 Gemini 专用的 SSE 帧解析（data JSON → StreamEvent），复用已有的 `extract_sse_frame` / `parse_sse_body` 框架。

**Files:**
- Modify: `rust/crates/brain-llm/src/stream.rs`

### Step 1: 写失败测试

在 `stream.rs` 的 `#[cfg(test)]` 模块追加（若该模块不存在则新建）：

```rust
    #[test]
    fn parse_gemini_sse_text_delta() {
        let frame = r#"data: {"candidates":[{"content":{"parts":[{"text":"你好"}]}}]}"#;
        let events = parse_single_gemini_data(frame).unwrap();
        assert!(matches!(
            events.first(),
            Some(StreamEvent::TextDelta { text }) if text == "你好"
        ));
    }

    #[test]
    fn parse_gemini_sse_thinking_delta() {
        let frame = r#"data: {"candidates":[{"content":{"parts":[{"text":"思考","thought":true}]}}]}"#;
        let events = parse_single_gemini_data(frame).unwrap();
        assert!(matches!(
            events.first(),
            Some(StreamEvent::ThinkingDelta { content }) if content == "思考"
        ));
    }

    #[test]
    fn parse_gemini_sse_function_call_start() {
        let frame = r#"data: {"candidates":[{"content":{"parts":[{"functionCall":{"name":"get_weather","args":{"city":"北京"}}}]}}]}"#;
        let events = parse_single_gemini_data(frame).unwrap();
        // functionCall 在 Gemini 流式里通常一次性完整出现
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCallStart { name, .. } if name == "get_weather")));
    }

    #[test]
    fn parse_gemini_sse_done_with_usage() {
        let frame = r#"data: {"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15}}"#;
        let events = parse_single_gemini_data(frame).unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Done { .. })));
    }
```

### Step 2: 跑测试验证失败

Run: `cargo test -p brain-llm stream::tests 2>&1 | tail -20`
Expected: 编译失败（`parse_single_gemini_data` 未定义）

### Step 3: 实现

在 `stream.rs` 顶部 `use` 补充（若缺）：

```rust
use crate::types::{FinishReason, StreamEvent};
```

新增函数（放在 `parse_single_sse_data` 之后）：

```rust
/// 解析单个 Gemini SSE data 帧（不含 `data: ` 前缀的 JSON）为 StreamEvent 列表。
///
/// Gemini 流式 JSON 结构：
/// - candidates[0].content.parts[].text  → TextDelta
/// - candidates[0].content.parts[].text(thought=true) → ThinkingDelta
/// - candidates[0].content.parts[].functionCall → ToolCallStart + ToolCallDelta
/// - candidates[0].finishReason + usageMetadata → Done
pub fn parse_single_gemini_data(data: &str) -> Result<Vec<StreamEvent>> {
    let value: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| crate::error::LlmError::RequestFailed(format!("Gemini SSE JSON 解析失败: {e}")))?;

    let mut events = Vec::new();
    let mut has_tool_use = false;

    if let Some(parts) = value
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|f| f.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
    {
        let mut call_seq = 0u32;
        for part in parts {
            let is_thought = part
                .get("thought")
                .and_then(|t| t.as_bool())
                .unwrap_or(false);
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if is_thought {
                    events.push(StreamEvent::ThinkingDelta {
                        content: text.to_string(),
                    });
                } else {
                    events.push(StreamEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
            }
            if let Some(fc) = part.get("functionCall") {
                if let Some(name) = fc.get("name").and_then(|n| n.as_str()) {
                    call_seq += 1;
                    let id = format!("call_{call_seq}");
                    let args = fc.get("args").cloned().unwrap_or(serde_json::json!({}));
                    events.push(StreamEvent::ToolCallStart {
                        id: id.clone(),
                        name: name.to_string(),
                    });
                    events.push(StreamEvent::ToolCallDelta {
                        tool_use_id: id,
                        delta: args.to_string(),
                    });
                    has_tool_use = true;
                }
            }
        }
    }

    // finishReason + usageMetadata → Done
    let finish = value
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|f| f.get("finishReason"))
        .and_then(|r| r.as_str());

    let usage = value.get("usageMetadata");
    if finish.is_some() || usage.is_some() {
        let fr = finish.map(|s| match s {
            "MAX_TOKENS" => FinishReason::MaxTokens,
            "STOP" if has_tool_use => FinishReason::ToolUse,
            "STOP" => FinishReason::EndTurn,
            _ if has_tool_use => FinishReason::ToolUse,
            _ => FinishReason::EndTurn,
        });
        let token_usage = usage.map(|u| crate::types::TokenUsage {
            prompt_tokens: u.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
            completion_tokens: u.get("candidatesTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
            total_tokens: u.get("totalTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        });
        events.push(StreamEvent::Done {
            finish_reason: fr,
            usage: token_usage,
        });
    }

    Ok(events)
}
```

### Step 4: 跑测试验证通过

Run: `cargo test -p brain-llm stream::tests 2>&1 | tail -20`
Expected: PASS（含 4 个新测试 + 既有 SSE 测试）

### Step 5: 提交

```bash
git add rust/crates/brain-llm/src/stream.rs
git commit -m "feat(brain-llm): Gemini SSE 流式帧解析"
```

---

## Task 7: GeminiClient impl LlmProvider

把前面的纯函数接到 `LlmProvider` trait，实现 `complete` / `stream_complete` / `stream_incremental`。HTTP 调用用 `SharedHttpClient`。

**Files:**
- Modify: `rust/crates/brain-llm/src/gemini.rs`

### Step 1: 写测试（URL/Header 构造验证）

impl 层的真实 HTTP 不做单测（需真实 API Key）。通过提取可测的辅助方法验证 URL 与 header 构造。
在 `mod tests` 追加：

```rust
    #[test]
    fn auth_header_is_x_goog_api_key() {
        let c = make_client();
        let headers = c.build_headers();
        let key_header = headers
            .iter()
            .find(|(k, _)| *k == "x-goog-api-key")
            .map(|(_, v)| v.as_str());
        assert_eq!(key_header, Some("test-key"));
    }

    #[test]
    fn auth_header_includes_content_type() {
        let c = make_client();
        let headers = c.build_headers();
        assert!(headers.iter().any(|(k, v)| *k == "Content-Type" && v == "application/json"));
    }
```

### Step 2: 跑测试验证失败

Run: `cargo test -p brain-llm gemini::tests 2>&1 | tail -20`
Expected: 编译失败（`build_headers` 未定义）

### Step 3: 实现

在 `gemini.rs` 顶部 `use` 补充：

```rust
use std::future::Future;
use std::pin::Pin;

use crate::provider::{ChatRequest as Req, ChatResponse as Resp, LlmProvider};
use crate::stream;
use crate::types::StreamEvent;
```

在 `impl GeminiClient` 新增 `build_headers`：

```rust
    /// 构造请求 header（可单测）
    fn build_headers(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Content-Type", "application/json".into()),
            ("x-goog-api-key", self.api_key.clone()),
        ]
    }
```

在文件末尾新增 trait 实现：

```rust
impl LlmProvider for GeminiClient {
    fn model(&self) -> &str {
        &self.model
    }

    fn complete(
        &self,
        request: Req,
    ) -> Pin<Box<dyn Future<Output = crate::error::Result<Resp>> + Send + '_>> {
        let url = self.generate_url();
        let body = self.to_gemini_request(request);
        let headers = self.build_headers();
        let fallback_model = self.model.clone();
        let retry = self.http.retry_config().clone();
        let client = self.http.client().clone();

        Box::pin(async move {
            let mut attempts = 0u32;
            let max_attempts = retry.max_retries + 1;
            loop {
                attempts += 1;
                let mut req = client.post(&url);
                for (k, v) in &headers {
                    req = req.header(*k, v);
                }
                let resp = req.json(&body).send().await;
                match resp {
                    Ok(response) => {
                        let status = response.status();
                        if !status.is_success() {
                            let text = response.text().await.unwrap_or_default();
                            let err = crate::error::LlmError::ApiError {
                                status: status.as_u16(),
                                message: text,
                            };
                            if err.is_retryable() && attempts < max_attempts {
                                tokio::time::sleep(retry.backoff_for_attempt(attempts)).await;
                                continue;
                            }
                            return Err(err);
                        }
                        let raw = response
                            .text()
                            .await
                            .map_err(|e| crate::error::LlmError::RequestFailed(e.to_string()))?;
                        let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                            crate::error::LlmError::RequestFailed(format!("响应解析失败: {e}"))
                        })?;
                        return Ok(Self::parse_gemini_response(value, fallback_model));
                    }
                    Err(e) => {
                        let err =
                            crate::error::LlmError::RequestFailed(format!("HTTP 请求失败: {e}"));
                        if err.is_retryable() && attempts < max_attempts {
                            tokio::time::sleep(retry.backoff_for_attempt(attempts)).await;
                            continue;
                        }
                        return Err(err);
                    }
                }
            }
        })
    }

    fn stream_complete(
        &self,
        request: Req,
    ) -> Pin<Box<dyn Future<Output = crate::error::Result<Vec<StreamEvent>>> + Send + '_>> {
        let url = self.stream_url();
        let body = self.to_gemini_request(request);
        let headers = self.build_headers();
        Box::pin(async move {
            stream::stream_gemini(self.http.client(), &url, &headers, &body).await
        })
    }

    fn stream_incremental(
        &self,
        request: Req,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = crate::error::Result<
                        tokio::sync::mpsc::Receiver<StreamEvent>,
                    >,
                > + Send
                + '_,
        >,
    > {
        let url = self.stream_url();
        let body = self.to_gemini_request(request);
        let headers = self.build_headers();
        Box::pin(async move {
            stream::stream_gemini_incremental(self.http.client(), &url, &headers, &body).await
        })
    }
}
```

同时需要在 `stream.rs` 新增 `stream_gemini` 和 `stream_gemini_incremental` 函数（基于 Task 6 的 `parse_single_gemini_data`）：

```rust
/// Gemini 批量流式：收集所有 SSE 事件
pub async fn stream_gemini(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, String)],
    body: &serde_json::Value,
) -> crate::error::Result<Vec<StreamEvent>> {
    let mut req = client.post(url);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let response = req.json(body).send().await.map_err(|e| {
        crate::error::LlmError::RequestFailed(format!("Gemini 流式请求失败: {e}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(crate::error::LlmError::ApiError {
            status: status.as_u16(),
            message: text,
        });
    }
    let full = response.text().await.map_err(|e| {
        crate::error::LlmError::RequestFailed(format!("读取 Gemini 流式响应失败: {e}"))
    })?;
    let mut events = Vec::new();
    for frame in parse_sse_body(&full)? {
        // parse_sse_body 已对 OpenAI 做了解析；Gemini 需要独立遍历 data 行
        // 这里改为：直接遍历 SSE data 段并用 Gemini 解析器
    }
    // 简化实现：遍历每个 SSE data 段
    for data in extract_all_sse_data(&full) {
        if let Ok(evts) = parse_single_gemini_data(&data) {
            events.extend(evts);
        }
    }
    Ok(events)
}

/// Gemini 增量流式：推入 channel
pub async fn stream_gemini_incremental(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, String)],
    body: &serde_json::Value,
) -> crate::error::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let mut req = client.post(url);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let response = req.json(body).send().await.map_err(|e| {
        crate::error::LlmError::RequestFailed(format!("Gemini 流式请求失败: {e}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(crate::error::LlmError::ApiError {
            status: status.as_u16(),
            message: text,
        });
    }
    // 使用 bytes_stream 增量读取
    use futures::StreamExt;
    let mut byte_stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    tokio::spawn(async move {
        while let Some(chunk_res) = byte_stream.next().await {
            if let Ok(chunk) = chunk_res {
                buffer.extend_from_slice(&chunk);
                while let Some(frame) = extract_sse_frame(&mut buffer) {
                    if let Some(data) = frame.strip_prefix("data: ") {
                        if let Ok(evts) = parse_single_gemini_data(data.trim()) {
                            for e in evts {
                                if tx.send(e).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    Ok(rx)
}

/// 从完整 SSE 文本中提取所有 data: 行内容
fn extract_all_sse_data(full: &str) -> Vec<String> {
    full.lines()
        .filter_map(|l| l.strip_prefix("data: ").map(|s| s.trim().to_string()))
        .collect()
}
```

> 注：`extract_sse_frame` 已存在于 `stream.rs`（处理 buffer 切帧），直接复用。
> `parse_sse_body` 旧实现针对 OpenAI，Gemini 路径用 `extract_all_sse_data` + `parse_single_gemini_data` 独立处理。

### Step 4: 跑测试验证通过

Run: `cargo test -p brain-llm gemini::tests 2>&1 | tail -20`
Expected: PASS（17 个测试，含本任务 2 个）

Run: `cargo clippy -p brain-llm --all-targets -- -D warnings 2>&1 | tail -20`
Expected: 无 warning

### Step 5: 提交

```bash
git add rust/crates/brain-llm/src/gemini.rs rust/crates/brain-llm/src/stream.rs
git commit -m "feat(brain-llm): GeminiClient 实现 LlmProvider trait + 流式"
```

---

## Task 8: create_brain_client 路由集成 + default_config 示例

让 `create_brain_client` 按 `kind` 分发到 `GeminiClient` / `OpenAiCompatClient`，并在 `default_config` 加入 gemini 示例 provider。

**Files:**
- Modify: `rust/crates/brain-llm/src/config.rs`

### Step 1: 写失败测试

在 `config.rs` 的 `mod tests` 追加：

```rust
    #[test]
    fn create_brain_client_routes_gemini_kind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(
            &path,
            r#"
[llm]
default_provider = "gemini"
default_model = "gemini-2.5-flash"

[llm.providers.gemini]
api_base = "https://generativelanguage.googleapis.com/v1beta"
api_key_env = "GEMINI_TEST_KEY_KIND"

[llm.brain_providers]
main = "gemini"
"#,
        )
        .unwrap();
        std::env::set_var("GEMINI_TEST_KEY_KIND", "fake-key");
        let cfg = LlmConfig::load(&path).unwrap();
        let client = cfg.create_brain_client("main").unwrap();
        // GeminiClient 的 model() 返回 gemini-2.5-flash
        assert_eq!(client.model(), "gemini-2.5-flash");
        std::env::remove_var("GEMINI_TEST_KEY_KIND");
    }

    #[test]
    fn create_brain_client_routes_openai_kind_by_default() {
        let cfg = LlmConfig::default_config();
        std::env::set_var("XIAOMI_API_KEY", "fake");
        let client = cfg.create_brain_client("main").unwrap();
        // OpenAI 路径，model 是 mimo-7b
        assert_eq!(client.model(), "mimo-7b");
        std::env::remove_var("XIAOMI_API_KEY");
    }

    #[test]
    fn create_brain_client_injects_proxy_into_gemini() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(
            &path,
            r#"
[proxy]
default = "http://127.0.0.1:7890"

[llm]
default_provider = "gemini"
default_model = "gemini-2.5-flash"

[llm.providers.gemini]
api_base = "https://generativelanguage.googleapis.com/v1beta"
api_key_env = "GEMINI_PROXY_TEST"
kind = "gemini"

[llm.brain_providers]
main = "gemini"
"#,
        )
        .unwrap();
        std::env::set_var("GEMINI_PROXY_TEST", "fake");
        let cfg = LlmConfig::load(&path).unwrap();
        // 应成功构造（代理注入不报错）
        let client = cfg.create_brain_client("main");
        assert!(client.is_ok());
        std::env::remove_var("GEMINI_PROXY_TEST");
    }
```

### Step 2: 跑测试验证失败

Run: `cargo test -p brain-llm config::tests::create_brain_client 2>&1 | tail -20`
Expected: FAIL（当前 create_brain_client 总是创建 OpenAiCompatClient，gemini 配置的 model 路由不对）

### Step 3: 实现

在 `config.rs` 顶部 `use` 补充：

```rust
use crate::gemini::GeminiClient;
```

替换 `create_brain_client` 方法整体：

```rust
    /// 为某个副脑构建 LLM 客户端
    pub fn create_brain_client(&self, brain_name: &str) -> Result<Box<dyn LlmProvider>> {
        let model = self.model_for_brain(brain_name);
        let provider_name = self.provider_for_brain(brain_name);
        let api_key = self.resolve_api_key(provider_name)?;
        let (max_tokens, temperature) = self.params_for_brain(brain_name);

        let provider_config = self
            .llm
            .providers
            .get(provider_name)
            .ok_or_else(|| LlmError::ProviderNotFound(provider_name.to_string()))?;

        let proxy_url = self.resolve_proxy(provider_name)?;

        match provider_config.kind {
            ProviderKind::OpenAi => {
                let client = OpenAiCompatClient::new(
                    provider_config.api_base.clone(),
                    api_key,
                    model.to_string(),
                    max_tokens,
                    temperature,
                )
                .with_proxy(proxy_url);
                Ok(Box::new(client))
            }
            ProviderKind::Gemini => {
                let client = GeminiClient::new(
                    provider_config.api_base.clone(),
                    api_key,
                    model.to_string(),
                    max_tokens,
                    temperature,
                    proxy_url,
                );
                Ok(Box::new(client))
            }
        }
    }
```

在 `default_config()` 的 `providers` 插入中加入 gemini 示例（在 zhipu 之后）：

```rust
        providers.insert(
            "gemini".into(),
            ProviderConfig {
                api_base: "https://generativelanguage.googleapis.com/v1beta".into(),
                api_key_env: "GEMINI_API_KEY".into(),
                api_key: None,
                kind: ProviderKind::Gemini,
                proxy: None,
            },
        );
```

> 注意：现有 `default_config()` 中各 ProviderConfig 字面量需补齐 `kind: ProviderKind::OpenAi, proxy: None` 两个字段
> （OpenAi 是 `#[default]`，但结构体字面量需显式写或用 `..Default::default()`。建议给 `ProviderConfig` 加 `Default` impl，
> 或逐个补字段。推荐后者，显式清晰。）

为简化，给 `ProviderConfig` 实现 `Default`：

```rust
impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            api_base: String::new(),
            api_key_env: String::new(),
            api_key: None,
            kind: ProviderKind::OpenAi,
            proxy: None,
        }
    }
}
```

并把 `default_config()` 中既有 provider 字面量改为结构体更新语法：

```rust
        providers.insert("xiaomi".into(), ProviderConfig {
            api_base: "https://xiaomi-llm.example.com/v1".into(),
            api_key_env: "XIAOMI_API_KEY".into(),
            ..Default::default()
        });
        // deepseek / zhipu 同理
```

### Step 4: 跑测试验证通过

Run: `cargo test -p brain-llm config::tests 2>&1 | tail -20`
Expected: PASS（含 3 个新测试 + 既有全过）

### Step 5: 提交

```bash
git add rust/crates/brain-llm/src/config.rs
git commit -m "feat(brain-llm): create_brain_client 按 kind 路由 + 代理注入"
```

---

## Task 9: OpenAiCompatClient with_proxy

给 `OpenAiCompatClient` 加 `with_proxy` 方法，使其也能走配置代理（国外 OpenAI 兼容 provider 共用）。

**Files:**
- Modify: `rust/crates/brain-llm/src/openai_compat.rs`

### Step 1: 写失败测试

在 `openai_compat.rs` 的 `mod tests` 追加：

```rust
    #[test]
    fn with_proxy_http_url_succeeds() {
        let client = OpenAiCompatClient::new(
            "https://api.example.com/v1".into(),
            "key".into(),
            "m".into(),
            1024,
            0.5,
        )
        .with_proxy(Some("http://127.0.0.1:7890".into()));
        // 构造成功即可（内部 reqwest::Client 已注入代理）
        assert_eq!(client.model(), "m");
    }

    #[test]
    fn with_proxy_none_keeps_direct() {
        let client = OpenAiCompatClient::new(
            "https://api.example.com/v1".into(),
            "key".into(),
            "m".into(),
            1024,
            0.5,
        )
        .with_proxy(None);
        assert_eq!(client.model(), "m");
    }

    #[test]
    fn with_proxy_invalid_url_falls_back_to_direct() {
        // 无效代理应回退直连而非 panic（保持系统可用性）
        let client = OpenAiCompatClient::new(
            "https://api.example.com/v1".into(),
            "key".into(),
            "m".into(),
            1024,
            0.5,
        )
        .with_proxy(Some("not-a-url".into()));
        assert_eq!(client.model(), "m");
    }
```

### Step 2: 跑测试验证失败

Run: `cargo test -p brain-llm openai_compat::tests::with_proxy 2>&1 | tail -20`
Expected: 编译失败（`with_proxy` 未定义）

### Step 3: 实现

在 `impl OpenAiCompatClient` 中（`with_retry_config` 之后）新增：

```rust
    /// 注入代理，返回新的 client（builder 模式）。
    ///
    /// 仅支持 `http://` 代理。无效地址回退直连（保持系统可用性）。
    #[must_use]
    pub fn with_proxy(mut self, proxy_url: Option<String>) -> Self {
        if let Some(url) = proxy_url {
            match reqwest::Proxy::all(&url) {
                Ok(proxy) => {
                    self.client = match reqwest::Client::builder()
                        .connect_timeout(std::time::Duration::from_secs(30))
                        .timeout(std::time::Duration::from_secs(300))
                        .proxy(proxy)
                        .build()
                    {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("代理 client 构造失败，回退直连: {e}");
                            self.client; // 保持原 client
                        }
                    };
                }
                Err(e) => {
                    tracing::warn!("无效代理地址 '{url}'，回退直连: {e}");
                }
            }
        }
        self
    }
```

> 注：`with_proxy` 无效时回退直连而非报错，与 `SharedHttpClient::new`（报错）策略不同。
> OpenAiCompatClient 保持向后兼容的「永不出错」语义；GeminiClient 走 SharedHttpClient 严格报错。

### Step 4: 跑测试验证通过

Run: `cargo test -p brain-llm openai_compat::tests 2>&1 | tail -20`
Expected: PASS（含 3 个新测试 + 既有全过）

### Step 5: 提交

```bash
git add rust/crates/brain-llm/src/openai_compat.rs
git commit -m "feat(brain-llm): OpenAiCompatClient 支持 with_proxy 注入代理"
```

---

## Task 10: lib.rs 导出 + 全量验证 + 文档

导出公共类型，运行全量 fmt/clippy/test，更新 CLAUDE 记忆。

**Files:**
- Modify: `rust/crates/brain-llm/src/lib.rs`

### Step 1: 修改 lib.rs 导出

```rust
pub mod config;
pub mod echo;
pub mod error;
pub mod gemini;
pub mod http_client;
pub mod openai_compat;
pub mod provider;
pub mod stream;
pub mod types;

pub use config::{LlmConfig, ProxySection, ProviderKind};
pub use error::{LlmError, Result};
pub use gemini::GeminiClient;
pub use openai_compat::{OpenAiCompatClient, RetryConfig};
pub use provider::{
    build_context_messages, ChatMessage, ChatRequest, ChatResponse, LlmProvider, MessageRole,
};
pub use types::{ContentBlock, FinishReason, StreamEvent, TokenUsage, ToolChoice, ToolDefinition};
```

### Step 2: 全量格式化与 lint

Run:
```bash
cd rust && cargo fmt
```
Expected: 无输出（或自动格式化）

Run:
```bash
cd rust && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30
```
Expected: 无 warning，编译通过

### Step 3: 全量测试

Run:
```bash
cd rust && cargo test --workspace 2>&1 | tail -40
```
Expected: 所有测试通过（brain-llm 新增约 25+ 测试，其他 crate 不受影响，brain-integration-tests 除外按既有约定）

### Step 4: 提交

```bash
git add rust/crates/brain-llm/src/lib.rs
git commit -m "feat(brain-llm): 导出 GeminiClient/ProviderKind/ProxySection + fmt"
```

### Step 5: 更新项目记忆

更新 `~/.claude/projects/-Users-chenh-RustObject-claw-code-parity/memory/MEMORY.md`，
在「当前 Crate 结构」brain-llm 描述补充：
```
brain-llm/      # LLM Provider trait + OpenAI 兼容客户端 + Gemini 原生客户端 + SharedHttpClient + 代理配置
```
并新增一节「Gemini 原生 Provider 适配（2026-06-18）」记录关键决策：
- ProviderConfig.kind 路由（openai/gemini）
- 全局 [proxy].default + provider 级 proxy 三态
- GeminiClient 独立 impl，functionCall 无 id 自生成 call_{n}
- 设计: docs/plans/2026-06-18-gemini-provider-design.md
- 实施: docs/plans/2026-06-18-gemini-provider-impl.md

---

## 完成标准

- [ ] 所有 10 个任务提交完成
- [ ] `cargo fmt` 无变更
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 无 warning
- [ ] `cargo test --workspace` 全绿（brain-integration-tests 按既有约定除外）
- [ ] brain-llm 新增测试 ≥ 25 个
- [ ] 上层 crate（brain-main/brain-memory/brain-eval/ai-brain-cli）零改动
- [ ] MEMORY.md 更新

## 端到端手动验证（实施完成后，可选）

```bash
# 1. 配置
export GEMINI_API_KEY=AIza...   # 从 aistudio.google.com 获取
# 若需代理：
export HTTPS_PROXY=http://127.0.0.1:7890

# 2. 编辑 ~/.ai-brain/config.toml 加 gemini provider（见设计文档 §4 示例）

# 3. 跑一次
cargo run -p ai-brain-cli -- chat  # 或 web 模式
# 输入消息，确认 Gemini 返回正常 + thinking 可见 + 工具调用正常
```
