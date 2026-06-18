# Gemini 原生 Provider 适配设计

- **日期**: 2026-06-18
- **状态**: 已批准（brainstorming 产出）
- **范围**: `brain-llm` crate 内部，上层逻辑零改动
- **关联**: 架构见 `docs/architecture/v2/`，配置见 `brain-llm/src/config.rs`

## 1. 背景与目标

当前 LLM 适配层只支持 OpenAI 兼容格式，`OpenAiCompatClient` 是唯一真实实现，
`ProviderConfig` 无协议类型字段，`create_brain_client` 硬编码创建 `OpenAiCompatClient`。

本次目标：在保留 OpenAI 路径不变的前提下，新增对 **Google Gemini 原生 API** 的支持，
使智脑可按脑（per-brain）路由到 Gemini，享受其免费层与 thinking 能力。

### 非目标（本次不做）

- OAuth 账号登录（仅 API Key；不预留 auth 枚举，YAGNI）
- socks5 代理（仅 `http://` 代理）
- 自建 Google Cloud OAuth 应用
- Anthropic / 其他协议适配

## 2. 关键决策记录

| 决策点 | 选择 | 理由 |
|-------|------|------|
| 接入方式 | 原生 Gemini API（非 OpenAI 兼容端点） | 功能最全：thinking、原生 function calling、长上下文 |
| 功能范围 | 全部（流式 + thinking + function calling） | TUI/Web 流式交互需完整可用 |
| 代理配置 | 写入配置文件（全局 default + provider 级三态） | 比环境变量可控；国外模型默认走、国内显式 `none` |
| OAuth | 不做、不预留 | 复杂度高 + 合规风险；Phase 2 独立做 |
| Client 组织 | 独立 `GeminiClient`（方案 A） | 协议差异大，合并会膨胀；共享 HTTP/重试/代理能力 |

## 3. 架构边界（不变量）

```
┌──────────────────────────────────────────────┐
│ 上层逻辑（完全与协议无关，本次不动）            │
│  conversation.rs / orchestrator.rs / prompts.rs │
│  memory_brain / eval_brain / 上下文压缩 / KV Cache │
└──────────────────┬───────────────────────────┘
                   │  ChatRequest  (Vec<ChatMessage>)
                   │  ChatResponse (Vec<ContentBlock>)
                   ▼
┌──────────────────────────────────────────────┐
│ LlmProvider trait   ← 唯一契约边界            │
└──────┬───────────────┬────────────┬──────────┘
       ▼               ▼            ▼
  OpenAiCompat     GeminiClient   EchoProvider
```

**所有上层逻辑零改动**。换 provider 对上层零感知，因为它们只认 `ChatRequest`/`ChatResponse`/`ContentBlock`。

## 4. 配置结构变更

所有新字段 `#[serde(default)]`，旧配置零改动向后兼容。

```rust
// 协议类型，决定路由
#[derive(Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]   // 缺省 = openai，旧配置无感
    OpenAi,
    Gemini,
}

pub struct ProviderConfig {
    pub api_base: String,
    #[serde(default)] pub api_key_env: String,
    #[serde(default)] pub api_key: Option<String>,
    #[serde(default)] pub kind: ProviderKind,        // 新增
    #[serde(default)] pub proxy: Option<String>,     // 新增：None/"none"/"http://..."
}

pub struct ProxySection {
    #[serde(default)] pub default: Option<String>,   // 全局默认代理
}

pub struct LlmConfig {
    pub llm: LlmSection,
    #[serde(default)] pub brain: BrainSection,
    #[serde(default)] pub proxy: ProxySection,       // 新增
    #[serde(default)] pub hooks: Option<toml::Value>,
}
```

`proxy` 字段三态用 `Option<String>` 表达（约定 `"none"` = 关闭），避免自定义 serde：

| 写法 | 语义 |
|------|------|
| 不写（`None`） | 跟随全局 `[proxy].default` |
| `Some("none")` | 显式关闭，强制直连 |
| `Some("http://...")` | 该 provider 独立代理地址 |

### 配置示例

```toml
[proxy]
default = "http://127.0.0.1:7890"

[llm]
default_provider = "gemini"
default_model = "gemini-2.5-flash"

[llm.providers.gemini]
api_base = "https://generativelanguage.googleapis.com/v1beta"
api_key_env = "GEMINI_API_KEY"
kind = "gemini"
# 不写 proxy → 跟随全局 default

[llm.providers.zhipu]
api_base = "https://open.bigmodel.cn/api/paas/v4"
api_key_env = "ZHIPU_API_KEY"
proxy = "none"   # 国内模型显式关闭代理
```

## 5. 路由与代理解析

`create_brain_client` 改为按 `kind` 分发：

```rust
match provider_config.kind {
    ProviderKind::OpenAi => Ok(Box::new(
        OpenAiCompatClient::new(...).with_proxy(self.resolve_proxy(provider_name)?)
    )),
    ProviderKind::Gemini => Ok(Box::new(
        GeminiClient::new(api_base, api_key, model, max_tokens, temperature, proxy)
    )),
}
```

```rust
fn resolve_proxy(&self, provider_name: &str) -> Result<Option<String>> {
    let pc = self.llm.providers.get(provider_name)?;
    Ok(match &pc.proxy {
        Some(s) if s == "none" => None,           // 显式关闭
        Some(s) => Some(s.clone()),               // 独立地址
        None => self.proxy.default.clone(),        // 跟随全局
    })
}
```

## 6. GeminiClient 结构

```
brain-llm/src/gemini.rs (新增, ~400 行)
├── pub struct GeminiClient {
│       api_base, api_key, model, max_tokens, temperature,
│       http: SharedHttpClient,
│   }
├── impl GeminiClient
│   ├── new / with_retry_config
│   ├── generate_url() → "{api_base}/models/{model}:generateContent"
│   ├── stream_url()   → "{api_base}/models/{model}:streamGenerateContent?alt=sse"
│   ├── to_gemini_request(ChatRequest) → Value
│   └── parse_gemini_response(Value) → ChatResponse
└── impl LlmProvider
    ├── complete          (非流式)
    ├── stream_complete   (batch SSE)
    └── stream_incremental (incremental SSE)
```

API Key 走 `x-goog-api-key` header（比 query param `?key=` 安全，不出现在日志 URL）。

## 7. 请求转换规则（ChatRequest → Gemini）

| ChatMessage 形态 | Gemini 映射 |
|-----------------|------------|
| 多个 system 消息 | **合并拼接**为单一 `systemInstruction.parts[{text}]`（Gemini 只允许一个） |
| user 纯文本 | `contents[].role=user, parts=[{text}]` |
| user 含 ToolResult | `contents[].role=user, parts=[{functionResponse:{name, response:{output:content}}}]` |
| assistant 纯文本 | `contents[].role=model, parts=[{text}]` |
| assistant 含 ToolUse | `contents[].role=model, parts=[{functionCall:{name, args:input}}]` |
| assistant 含 Thinking | 请求侧丢弃（Gemini 不回传 thinking） |

- 工具声明：`tools:[{functionDeclarations:[{name,description,parameters}]}]`
- 工具选择：`tool_choice` → `toolConfig.functionCallingConfig.mode`（AUTO/NONE/ANY）
- 生成参数：`generationConfig:{maxOutputTokens, temperature}`
- thinking 模型内部放大 `maxOutputTokens`（思考 token 也计入预算）

## 8. 响应解析规则（Gemini → ChatResponse）

```json
{"candidates":[{"content":{"parts":[...]},"finishReason":"STOP"}],"usageMetadata":{...}}
```

| part 类型 | ContentBlock 映射 |
|----------|------------------|
| `{text}` 或 `{text, thought:false}` | `Text` |
| `{text, thought:true}` | `Thinking` |
| `{functionCall:{name, args}}` | `ToolUse { id: 自生成, name, input: args }` |

**关键差异**：Gemini 的 `functionCall` **不带 id**。GeminiClient 内部按出现序生成 `call_{n}`，
请求侧用 `name` 匹配 `functionResponse`（Gemini 原生按 name 关联，不按 id）。

`finishReason` 映射：`STOP`→EndTurn, `MAX_TOKENS`→MaxTokens, 含 functionCall→ToolUse。

## 9. 流式解析

新增 `stream::stream_gemini` + `stream_gemini_incremental`，与 `stream_openai` 并列。
SSE 帧格式相同（`data: {json}\n\n`），复用已有 `extract_sse_frame` + `parse_sse_body`，
只新增 Gemini 专用的「data JSON → StreamEvent」解析。

| | OpenAI SSE | Gemini SSE |
|--|-----------|-----------|
| 文本增量 | `choices[0].delta.content` | `candidates[0].content.parts[0].text` |
| 思考增量 | `choices[0].delta.reasoning_content` | `parts[0].text`（`thought:true`） |
| 工具调用 | `choices[0].delta.tool_calls[]` 分片拼接 | `parts[0].functionCall`（通常一次性完整） |
| 结束 | `finish_reason` + `usage` | 末帧 `finishReason` + `usageMetadata` |

## 10. 共享 HTTP 客户端

新增 `brain-llm/src/http_client.rs`，封装 reqwest + 代理 + 重试：

```rust
pub struct SharedHttpClient {
    client: reqwest::Client,   // 构造时注入 proxy
    retry: RetryConfig,
}
impl SharedHttpClient {
    pub fn new(proxy_url: Option<&str>, retry: RetryConfig) -> Result<Self>;
    pub async fn post_json(&self, url, headers: Vec<(&str,String)>, body: Value)
        -> Result<reqwest::Response>;
    // 内部统一处理重试、超时、可重试错误判定
}
```

- **GeminiClient**：直接使用 `SharedHttpClient`
- **OpenAiCompatClient**：本次只加 `with_proxy(Option<String>)`（重建内部 reqwest::Client 注入代理），
  重试逻辑暂不动（最小改动，避免回归）。后续可统一迁移到 `SharedHttpClient`，不在本次范围。

代理注入：`reqwest::Client::builder().proxy(reqwest::Proxy::all(url)?)`，仅支持 `http://`。

## 11. 测试策略

全部用构造数据，**零真实网络调用**：

| 模块 | 测试点 |
|------|-------|
| `config.rs` | kind/proxy 解析、旧配置无 kind/proxy 向后兼容、resolve_proxy 三态、default_config 含 gemini 示例 |
| `gemini.rs` | to_gemini_request：多 system 合并、纯文本、tool_use、tool_result 各组合；parse_gemini_response：text/thinking/functionCall 各 part、finishReason 映射、functionCall 无 id 自生成 |
| `stream.rs` | stream_gemini：喂入模拟 Gemini SSE 帧，断言 StreamEvent 序列；含 thought part 增量 |
| `provider.rs` | create_brain_client 按 kind 正确分发 |

## 12. 文件清单与改动范围

| 文件 | 改动 |
|------|------|
| `brain-llm/src/config.rs` | 改：ProviderConfig +kind/proxy，LlmConfig +proxy 段，ProviderKind 枚举，resolve_proxy，create_brain_client 路由，default_config 加 gemini 示例 |
| `brain-llm/src/gemini.rs` | **新增**：GeminiClient + 请求/响应/SSE 转换 |
| `brain-llm/src/http_client.rs` | **新增**：SharedHttpClient |
| `brain-llm/src/stream.rs` | 改：新增 stream_gemini / stream_gemini_incremental + Gemini SSE 解析 |
| `brain-llm/src/openai_compat.rs` | 改：新增 `with_proxy(Option<String>)` |
| `brain-llm/src/lib.rs` | 改：导出 GeminiClient / ProviderKind / ProxySection |
| workspace `Cargo.toml` | 无需新依赖（reqwest 已有；socks 留 Future） |

**不动**：brain-main / brain-memory / brain-eval / brain-evolver / ai-brain-cli / orchestrator / conversation / prompts 全部零改动。

## 13. 风险与缓解

| 风险 | 缓解 |
|------|------|
| Gemini functionCall 无 id，多轮工具调用关联错乱 | 内部 `call_{n}` 伪 id + name 匹配（Gemini 原生语义） |
| thinking 占用 maxOutputTokens 致输出截断 | thinking 模型内部放大预算 |
| Gemini 区域限制 / 免费层 RPM 触顶 | 运行时问题，非代码；文档注明 + 代理配置可解网络 |
| 代理 socks5 不可用 | 文档注明仅 `http://`；socks 留 Future |
| OpenAiCompatClient 改动引入回归 | 仅加 `with_proxy`，重试逻辑不动；既有测试全部保留 |

## 14. Future Work（不在本次范围）

- **Phase 2 OAuth 登录**：独立 `gemini-oauth` 子模块，支持 Google 账号 PKCE 登录，
  对接 Gemini Code Assist 配额池（额度高于 API Key 免费层）。届时 ProviderConfig 引入 auth 枚举。
- **socks5 代理**：reqwest 加 `socks` feature。
- **OpenAiCompatClient 迁移到 SharedHttpClient**：统一 HTTP/重试层。
- **thoughtSignature 支持**：Gemini continue thinking 场景。
