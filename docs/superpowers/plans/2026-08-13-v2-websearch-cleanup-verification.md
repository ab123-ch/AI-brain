# 智脑 v2 WebSearch、Evolver、桩清理与验收 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 WebSearch/WebFetch 变成有界、可观测、可切换的真实动态后端，让 Evolver 复用同一 registry route，删除生产可达的假成功工具，并通过管理 API、静态扫描、完整 Rust 门禁和真实 release 冒烟完成交付。

**Architecture:** `tools::web` 定义稳定输入/输出、typed error、HTTP client 和 `SearchBackend`；`WebSearchProvider` 按显式优先级把稳定 `WebSearch`/`WebFetch` 门面绑定到 builtin/MCP/HTTP 后端。`RegistrySearchCapability` 固定请求快照并把 route 输出转换给 Evolver。Web/API 只展示 `ToolRuntimeStatus` 和触发受保护的 reload；清理阶段删除所有生产假 route，最后构建并替换当前运行的 release 服务。

**Tech Stack:** Rust 2021、Tokio、reqwest 0.12/rustls、axum 0.8、serde/serde_json、现有 tools/brain-evolver/brain-mcp/ai-brain-cli crates。

---

## 文件结构

- Create: `rust/crates/tools/src/web.rs`
- Modify: `rust/crates/tools/src/lib.rs`
- Modify: `rust/crates/tools/src/provider.rs`
- Modify: `rust/crates/tools/Cargo.toml`
- Modify: `rust/crates/brain-evolver/src/web_search.rs`
- Modify: `rust/crates/brain-evolver/src/cycle_runner.rs`
- Modify: `rust/crates/brain-evolver/src/coordinator.rs`
- Modify: `rust/crates/brain-evolver/src/error.rs`
- Modify: `rust/crates/brain-evolver/src/lib.rs`
- Modify: `rust/crates/brain-evolver/Cargo.toml`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`
- Modify: `rust/crates/ai-brain-cli/src/tool_runtime.rs`
- Modify: `rust/crates/ai-brain-cli/src/api_server.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/ws_handler.rs`
- Modify: `rust/crates/ai-brain-cli/src/main.rs`
- Modify: `rust/crates/ai-brain-cli/src/command/mod.rs`
- Create: `rust/crates/ai-brain-cli/src/command/tools_cmd.rs`
- Modify: `rust/crates/ai-brain-cli/Cargo.toml`
- Modify: `rust/crates/ai-brain-cli/src/real_tool_executor.rs`
- Modify: `rust/crates/brain-integration-tests/tests/dynamic_tools.rs`
- Create: `rust/scripts/smoke-v2-dynamic-tools.ps1`
- Review only: `src/`
- Review only: `tests/test_porting_workspace.py`

## 固定配置和错误合同

搜索配置：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WebCapabilityConfig {
    pub search_backends: Vec<SearchBackendConfig>,
    pub fetch: WebFetchConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchBackendConfig {
    BuiltinDdg { base_url: Option<String> },
    Mcp {
        server: String,
        tool: String,
        #[serde(default)]
        trusted_read_only: bool,
    },
    Http { name: String, base_url: String, auth_header_env: Option<String> },
}
```

`CLAWD_WEB_SEARCH_BASE_URL` 仅在没有正式 backend 配置时转成单个 `BuiltinDdg { base_url: Some(...) }` 以保持兼容；它不覆盖显式配置。

错误分类：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebErrorKind {
    InvalidInput,
    PermissionDenied,
    Timeout,
    Transport,
    RateLimited,
    Server,
    ResponseTooLarge,
    InvalidContent,
    Backend,
    Unavailable,
}
```

只有 `Timeout | Transport | RateLimited | Server` 可进入 fallback；其余错误立即返回。

### Task 1: 从 tools 巨型文件提取稳定 Web 类型与有界 HTTP 客户端

**Files:**
- Create: `rust/crates/tools/src/web.rs`
- Modify: `rust/crates/tools/src/lib.rs`
- Modify: `rust/crates/tools/Cargo.toml`

- [ ] **Step 1: 写 HTTP 边界 RED 测试**

在 `web.rs` 测试内启动本地 TcpListener/axum fixture，显式 no-proxy：

```rust
#[tokio::test]
async fn non_success_status_is_an_error_before_body_parsing() {
    let server = http_fixture(404, "<html>not found</html>").await;
    let input = WebFetchInput { url: server.url(), prompt: "summary".into() };
    let error = WebHttpClient::for_test_no_proxy(limits()).fetch(&input).await.unwrap_err();
    assert_eq!(error.kind(), WebErrorKind::InvalidInput);
    assert!(error.to_string().contains("404"));
}

#[tokio::test]
async fn oversized_body_stops_at_limit() {
    let server = chunked_fixture(200, 1_100_000).await;
    let input = WebFetchInput { url: server.url(), prompt: "summary".into() };
    let error = WebHttpClient::for_test_no_proxy(WebLimits { max_body_bytes: 1_000_000, ..limits() })
        .fetch(&input).await.unwrap_err();
    assert_eq!(error.kind(), WebErrorKind::ResponseTooLarge);
    assert!(server.bytes_sent().await < 1_100_000);
}

#[tokio::test]
async fn redirect_loop_and_timeout_are_typed() {
    assert_eq!(redirect_loop_client().await.unwrap_err().kind(), WebErrorKind::Transport);
    assert_eq!(slow_client().await.unwrap_err().kind(), WebErrorKind::Timeout);
}

#[test]
fn localhost_proxy_test_explicitly_uses_no_proxy() {
    let config = ProxyTestGuard::localhost();
    assert!(config.no_proxy_hosts().contains(&"127.0.0.1"));
    assert!(config.no_proxy_hosts().contains(&"localhost"));
}
```

再移植现有 HTML search、domain allow/block、DDG redirect decode 和 WebFetch title/summary 测试，确保输出兼容。

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p tools web_http --lib -- --nocapture`

Expected: 新 module/type 不存在。

- [ ] **Step 3: 实现异步、有界 Web client**

公开类型至少包含：

```rust
#[derive(Debug, Clone)]
pub struct WebLimits {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub redirect_limit: usize,
    pub max_body_bytes: usize,
}

#[derive(Clone)]
pub struct WebHttpClient { client: reqwest::Client, limits: WebLimits }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebFetchInput { pub url: String, pub prompt: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchInput {
    pub query: String,
    pub allowed_domains: Option<Vec<String>>,
    pub blocked_domains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebFetchOutput {
    pub bytes: usize,
    pub code: u16,
    #[serde(rename = "codeText")]
    pub code_text: String,
    pub result: String,
    #[serde(rename = "durationMs")]
    pub duration_ms: u128,
    pub url: String,
    pub content_type: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchOutput {
    pub query: String,
    pub results: Vec<WebSearchResultItem>,
    #[serde(rename = "durationSeconds")]
    pub duration_seconds: f64,
    pub backend: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WebSearchResultItem {
    Commentary(String),
    SearchResult { title: String, url: String, snippet: String },
}

impl WebHttpClient {
    pub async fn search_ddg(&self, input: &WebSearchInput, base_url: Option<&str>)
        -> Result<WebSearchOutput, WebError>;
    pub async fn fetch(&self, input: &WebFetchInput)
        -> Result<WebFetchOutput, WebError>;
}
```

实现规则：

- async reqwest + rustls，connect 5s、总请求 20s、redirect 5、body 默认 2 MiB。
- `error_for_status` 等价的显式分类先于 body 解析：400/404 InvalidInput，401/403 PermissionDenied，429 RateLimited，5xx Server，其余非 2xx Transport。
- 使用 `bytes_stream()` 增量累计，越界立即停止；输出标记 `truncated=false`，因为越界是错误而不是静默截断。
- WebFetch 输出保留 code/code_text/url/bytes/result/duration_ms，新增 content_type；只宣称 HTML/text 转换，不宣称运行 JavaScript。
- Search 输出保持 query/results/duration_seconds；空真实结果仍是成功空结果，后端错误不能变成空结果。
- reqwest 默认尊重系统 proxy 和 NO_PROXY。测试 client 使用 `.no_proxy()`，测试进程环境修改必须串行 guard 并恢复。
- WebFetch 在每次 DNS 解析和每跳 redirect 前执行 SSRF 校验：拒绝 loopback、link-local、私网、未指定端口策略和 DNS rebinding；测试 fixture 只能通过 `for_test_no_proxy` 注入显式允许的 loopback resolver，生产构造器没有该豁免。
- 从 `lib.rs` 删除 blocking Client 和旧 `execute_web_*` 实现；兼容 `execute_tool` 如仍被旧 CLI 调用，则用 `tokio::runtime::Handle` 是禁止的：改为 async route，旧 sync 入口对 Web 工具返回明确“请使用动态异步 route”错误，生产不调用该入口。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p tools web_http --lib -- --nocapture
cargo test -p tools web_search --lib -- --nocapture
cargo test -p tools web_fetch --lib -- --nocapture
cargo clippy -p tools --all-targets -- -D warnings
```

Expected: 状态码、大小、redirect、timeout、proxy、旧解析输出全部通过。

```powershell
git add crates/tools/Cargo.toml crates/tools/src/lib.rs crates/tools/src/web.rs
git commit -m "fix(web): bound search and fetch http behavior"
```

### Task 2: 实现搜索后端链与严格 fallback

**Files:**
- Modify: `rust/crates/tools/src/web.rs`
- Modify: `rust/crates/tools/src/provider.rs`

- [ ] **Step 1: 写后端选择与错误分类 RED 测试**

```rust
#[tokio::test]
async fn explicit_backend_order_is_stable_and_retryable_error_falls_back() {
    let first = Arc::new(RecordingBackend::error("mcp", WebErrorKind::Timeout));
    let second = Arc::new(RecordingBackend::success("ddg", one_hit()));
    let chain = SearchBackendChain::new(vec![first.clone(), second.clone()]);
    let output = chain.search(input("rust")).await.unwrap();
    assert_eq!(output.backend, "ddg");
    assert_eq!(first.calls(), 1);
    assert_eq!(second.calls(), 1);
}

#[tokio::test]
async fn invalid_input_permission_and_content_errors_never_fall_back() {
    for kind in [WebErrorKind::InvalidInput, WebErrorKind::PermissionDenied, WebErrorKind::InvalidContent] {
        let fixture = two_backend_fixture(kind);
        assert_eq!(fixture.chain.search(input("x")).await.unwrap_err().kind(), kind);
        assert_eq!(fixture.second.calls(), 0);
    }
}

#[tokio::test]
async fn all_unhealthy_backends_make_websearch_unavailable() {
    let provider = WebSearchProvider::new(unhealthy_backends(), fetch_backend());
    let contribution = provider.discover().await.unwrap();
    assert!(!names(&contribution).contains("WebSearch"));
    assert_eq!(contribution.health.state, ProviderState::Degraded);
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p tools search_backend --lib -- --nocapture`

Expected: backend chain/provider 尚不存在。

- [ ] **Step 3: 实现对象安全 backend 和 facade route**

```rust
pub trait SearchBackend: Send + Sync {
    fn id(&self) -> &str;
    fn health(&self) -> BackendHealth;
    fn search<'a>(&'a self, input: &'a WebSearchInput)
        -> WebFuture<'a, Result<WebSearchOutput, WebError>>;
}
```

同文件定义 `pub type WebFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;`。`WebError` 保存 `WebErrorKind`、sanitized message 和 backend id；其 `Debug/Display` 不输出认证 header。

```rust
#[derive(Debug, Clone, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct WebError {
    kind: WebErrorKind,
    message: String,
    backend_id: Option<String>,
}

impl WebError {
    pub fn kind(&self) -> WebErrorKind { self.kind }
    pub fn backend_id(&self) -> Option<&str> { self.backend_id.as_deref() }
}
```

构造器只接收脱敏 message；HTTP response body、auth env 值和完整 URL query 不进入错误文本。

实现三种 adapter：

- `DdgSearchBackend(WebHttpClient, base_url)`。
- `McpSearchBackend(pool, qualified_tool_name, output_mapper)`，调用子计划 C 的真实 pool；映射器接受常见 MCP `{results:[{title,url,snippet}]}` 和 content text JSON，无法解析为 InvalidContent。
- `ConfiguredHttpSearchBackend`，只访问配置的 base URL，query 参数固定 `q`，auth 从 env resolve 且脱敏。

`SearchBackendChain` 每次调用固定 backend Vec；逐项记录 sanitized attempt（backend id、duration、kind），只对允许 kind fallback，每 backend 一次，总尝试数不超过配置长度。它不基于发现顺序排序。

`McpSearchBackend` 在 Provider discovery 时固定子计划 C 的 server generation/raw route，不在执行时回查 pool 最新连接。`trusted_read_only=false` 时要求 MCP annotations 同时满足 `readOnlyHint=true` 且 `destructiveHint!=true`；否则该 backend 不允许挂到稳定只读 `WebSearch` 门面并产生诊断。`trusted_read_only=true` 是本地管理员的显式风险声明，仍只把固定 query/domain 字段映射给目标工具，不能转发任意模型参数。

`WebSearchProvider` provider id `builtin:web`：至少有一个通过上述信任检查的健康 chain 时发布 `WebSearch`；fetch client 健康时发布 `WebFetch`；两者 descriptor 取兼容 catalog，metadata network_access=true、ReadOnly、Medium，WebFetch guard profile 为 `RemoteUrl`。contribution 的 generation 是配置规范化结果、后端顺序、每个 MCP 固定连接 generation、HTTP/DDG endpoint 和信任设置的稳定哈希，并通过 `.with_generation(...)` 提交；route 直接 await async backend，不 spawn_blocking。这样后端 route 更换但 facade schema 未变时仍发布新快照，无变化 reload 则不递增 version。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p tools search_backend --lib -- --nocapture
cargo test -p tools web_provider --lib -- --nocapture
cargo clippy -p tools --all-targets -- -D warnings
```

Expected: fallback、MCP mapping、显式顺序、全部不可用撤销工具测试通过。

```powershell
git add crates/tools/src/provider.rs crates/tools/src/web.rs
git commit -m "feat(web): inject configurable search backends"
```

### Task 3: 将 Web provider 动态装入 ToolRuntime

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/tool_runtime.rs`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`
- Modify: `rust/crates/brain-integration-tests/tests/dynamic_tools.rs`

- [ ] **Step 1: 写 MCP 出现/断线后的门面重绑 RED 测试**

```rust
#[tokio::test]
async fn websearch_rebinds_on_new_snapshot_without_changing_old_request_route() {
    let fixture = web_runtime_with_ddg().await;
    let old = fixture.registry().snapshot();
    assert_eq!(fixture.backend_id_from(&old, "WebSearch"), "builtin_ddg");
    fixture.connect_search_mcp().await;
    fixture.runtime().reload(ReloadScope::Mcp).await.assert_success();
    let new = fixture.registry().snapshot();
    assert_eq!(fixture.backend_id_from(&old, "WebSearch"), "builtin_ddg");
    assert_eq!(fixture.backend_id_from(&new, "WebSearch"), "mcp:open-websearch/search");
}

#[tokio::test]
async fn no_initialized_search_backend_means_no_advertised_websearch() {
    let fixture = web_runtime_without_search().await;
    assert!(fixture.new_request_view().snapshot().resolve("WebSearch").is_none());
    assert!(fixture.runtime().status().providers.iter().any(|p| p.id == "builtin:web" && p.error.is_some()));
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-integration-tests --test dynamic_tools websearch -- --nocapture`

Expected: ToolRuntime 尚未组装 web provider。

- [ ] **Step 3: 实现配置解析和 refresh 联动**

- ToolRuntime 从统一 config 解析 WebCapabilityConfig；没有正式配置时使用兼容 env，再默认 builtin_ddg。
- MCP backend 只有 pool snapshot 中目标 server/tool 已连接时加入 chain。
- refresh 顺序为 MCP → Web contribution，确保同一次 reload 使用新 MCP 状态；MCP 断线后再次重建 Web contribution，按配置 fallback 或撤销。
- web provider I/O 健康探测只做配置校验/连接 client 构建，不在 registry lock 内发网络请求。
- Orchestrator 不再在 `RealToolExecutor` 静态 list 中暴露 WebSearch/WebFetch。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p brain-integration-tests --test dynamic_tools websearch -- --nocapture
cargo test -p ai-brain-cli tool_runtime --lib -- --nocapture
cargo clippy -p ai-brain-cli --all-targets -- -D warnings
```

Expected: 旧/新 snapshot route 固定、连接/断线重绑和不可用撤销通过。

```powershell
git add crates/ai-brain-cli/src/orchestrator.rs crates/ai-brain-cli/src/tool_runtime.rs crates/brain-integration-tests/tests/dynamic_tools.rs
git commit -m "feat(cli): refresh dynamic web capabilities"
```

### Task 4: Evolver 改为异步 registry SearchCapability

**Files:**
- Modify: `rust/crates/brain-evolver/src/web_search.rs`
- Modify: `rust/crates/brain-evolver/src/cycle_runner.rs`
- Modify: `rust/crates/brain-evolver/src/coordinator.rs`
- Modify: `rust/crates/brain-evolver/src/error.rs`
- Modify: `rust/crates/brain-evolver/src/lib.rs`
- Modify: `rust/crates/brain-evolver/Cargo.toml`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

- [ ] **Step 1: 写真实 route、缺能力和事件循环 RED 测试**

```rust
#[tokio::test]
async fn registry_search_capability_executes_web_routes_from_one_snapshot() {
    let route = Arc::new(RecordingWebRoute::results(one_hit(), page()));
    let snapshot = snapshot_with_web_routes(route.clone());
    let capability = RegistrySearchCapability::new(snapshot, context(), SearchBudget::default());
    assert_eq!(capability.search("rust", 5).await.unwrap()[0].title, "Rust");
    assert!(capability.fetch_page("https://example.test").await.unwrap().content.contains("body"));
    assert_eq!(route.snapshot_versions(), vec![snapshot.version(), snapshot.version()]);
}

#[tokio::test]
async fn missing_websearch_is_capability_unavailable_not_empty_success() {
    let capability = RegistrySearchCapability::new(empty_snapshot(), context(), SearchBudget::default());
    let error = capability.search("rust", 5).await.unwrap_err();
    assert!(matches!(error, EvolverError::CapabilityUnavailable(name) if name == "WebSearch"));
}

#[tokio::test]
async fn phase_research_does_not_block_tokio_runtime() {
    let search = Arc::new(DelayedAsyncSearch::new(Duration::from_millis(40)));
    let ticker = tokio::spawn(async { tokio::time::sleep(Duration::from_millis(5)).await; 1 });
    runner_with(search).phase_research(&perceive_fixture()).await.unwrap();
    assert_eq!(ticker.await.unwrap(), 1);
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-evolver web_search --lib -- --nocapture`

Expected: 同步 trait 与 StubWebSearch 不满足新测试。

- [ ] **Step 3: 改为对象安全 async trait 并删除生产 stub**

```rust
pub trait WebSearch: Send + Sync {
    fn search<'a>(&'a self, query: &'a str, max_results: usize)
        -> WebSearchFuture<'a, Result<Vec<SearchResult>>>;
    fn fetch_page<'a>(&'a self, url: &'a str)
        -> WebSearchFuture<'a, Result<PageContent>>;
    fn fetch_pages<'a>(&'a self, urls: &'a [String])
        -> WebSearchFuture<'a, Result<Vec<PageContent>>>;
}
```

同文件定义 `pub type WebSearchFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;`。

- Mock 实现改为 `Box::pin(async move { ... })`，仍只在测试使用。
- 删除 `StubWebSearch` 及所有默认构造调用；`CycleRunner::new/with_memory` 改返回 `Result<Self>` 并要求显式 search，或仅在 `#[cfg(test)]` helper 注入 Mock。生产构造必须传能力。
- `RegistrySearchCapability` 持有 `Arc<ToolSnapshot>`、ToolExecutionContext 和 budget；解析 WebSearch/WebFetch JSON 输出，route error 映射。
- `RegistrySearchCapability` 在执行前从固定 snapshot resolve `WebSearch`/`WebFetch`，分别用 `ToolRequestView::base(snapshot.clone()) + ToolAuthorizationPolicy::read_only() + ToolApproval::None` 调共享 `authorize_call`；不在广告集合、非 ReadOnly 或需要确认都返回明确错误，绝不直接调用 route 绕过 policy。
- `SearchBudget { max_queries: 8, max_fetches: 12, max_total_bytes: 8 * 1024 * 1024, deadline: Duration::from_secs(120) }`；内部 `Mutex<SearchBudgetState>` 原子扣减 query/fetch/bytes，外层 `tokio::time::timeout_at` 限制总耗时。预算越界为 `EvolverError::PermissionDenied`，能力缺失为 `CapabilityUnavailable`。
- `phase_research` 对 search/fetch 全部 `.await`，禁止 `block_on`。
- skills_dir 从 Orchestrator 的 AiBrainPaths 注入，删除 HOME/`.` 回退。
- SharedResources 删除 `mcp_pool_info: String`，增加 registry snapshot/search capability 的强类型引用。

- [ ] **Step 4: Orchestrator 注入同一 registry 能力**

`spawn_evolution` 在启动一次 evolution 前固定 `tool_runtime.registry().snapshot()`，创建 `RegistrySearchCapability`；没有 WebSearch 时立即返回中文 capability unavailable，不创建后台“成功空研究”。

- [ ] **Step 5: GREEN 和提交**

Run:

```powershell
cargo test -p brain-evolver web_search --lib -- --nocapture
cargo test -p brain-evolver cycle_runner --lib -- --nocapture
cargo test -p ai-brain-cli evolver --lib -- --nocapture
cargo clippy -p brain-evolver -p ai-brain-cli --all-targets -- -D warnings
```

Expected: async、缺能力、预算和原进化研究测试通过；源码无 StubWebSearch。

```powershell
git add crates/brain-evolver/Cargo.toml crates/brain-evolver/src/coordinator.rs crates/brain-evolver/src/cycle_runner.rs crates/brain-evolver/src/error.rs crates/brain-evolver/src/lib.rs crates/brain-evolver/src/web_search.rs crates/ai-brain-cli/src/orchestrator.rs
git commit -m "refactor(evolver): use registry web capabilities"
```

### Task 5: 删除生产假工具和静态 ToolSearch 真相源

**Files:**
- Modify: `rust/crates/tools/src/lib.rs`
- Modify: `rust/crates/tools/src/provider.rs`
- Modify: `rust/crates/ai-brain-cli/src/real_tool_executor.rs`
- Modify: `rust/crates/brain-integration-tests/tests/dynamic_tools.rs`

- [ ] **Step 1: 写排除与假成功 RED 测试**

```rust
#[test]
fn production_catalog_has_no_fake_capability_specs() {
    let names = production_compatible_specs().into_iter().map(|s| s.name).collect::<BTreeSet<_>>();
    for absent in ["LSP", "RemoteTrigger", "MCP", "TestingPermission"] {
        assert!(!names.contains(absent));
    }
}

#[tokio::test]
async fn no_backend_never_returns_empty_or_triggered_success() {
    let fixture = DynamicRuntimeFixture::without_external_backends().await;
    for name in ["LSP", "RemoteTrigger", "MCP", "ListMcpResources", "ReadMcpResource", "McpAuth"] {
        assert!(fixture.registry().snapshot().resolve(name).is_none());
    }
}

#[test]
fn testing_permission_can_only_be_added_by_test_session_provider() {
    assert!(production_snapshot().resolve("TestingPermission").is_none());
    assert!(test_session_snapshot().resolve("TestingPermission").is_some());
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-integration-tests --test dynamic_tools fake_tools -- --nocapture`

Expected: 旧 catalog/execute branches 仍暴露假能力。

- [ ] **Step 3: 删除假实现与旧静态搜索**

从 tools 生产代码删除：

- LSP spec/input/run_lsp（本次不造 LSP provider）。
- RemoteTrigger spec/input/run_remote_trigger（保留未来 provider 设计，不接受任意 URL）。
- 通用 MCP spec/input/run_mcp_tool。
- TestingPermission spec/input/run_testing_permission；测试改用 SessionProvider route。
- 旧 run_list/read_mcp_resource 与 run_mcp_auth；真实版本已在 brain-mcp。
- `execute_tool_search`、`search_tool_specs` 和依赖 `deferred_tool_specs()` 的生产调用。compat catalog 可保留 descriptor lookup，但 ToolSearch 只在 brain-main RequestView 实现。

AskUserQuestion 的底层 `run_ask_user_question` 假 pending JSON 也删除；只有 SystemToolRoute 能执行该名字。`execute_tool("AskUserQuestion", ...)` 返回明确错误，生产 route 不会调用它。

`mvp_tool_specs` 重命名为 `compatible_builtin_specs` 并保持 deprecated re-export 仅供旧非 v2 测试；其中只包含有真实内置实现或被真实 provider 复用 Schema 的工具。Novel specs 仍停用，不被生产 provider 引用。

- [ ] **Step 4: GREEN 与静态扫描**

Run:

```powershell
cargo test -p tools --lib -- --nocapture
cargo test -p brain-integration-tests --test dynamic_tools fake_tools -- --nocapture
rg -n -i 'not yet implemented|stub response|MCP tool proxy not yet connected|LSP server not connected|No MCP resources available|Testing permission tool stub' crates/tools/src crates/brain-mcp/src crates/brain-motor/src crates/brain-evolver/src crates/ai-brain-cli/src
```

Expected: tests 通过；`rg` 无输出。测试 mock/stub 允许存在于 `#[cfg(test)]`，但上述扫描范围内的这些具体生产文案必须消失。

- [ ] **Step 5: 提交**

```powershell
git add crates/tools/src/lib.rs crates/tools/src/provider.rs crates/ai-brain-cli/src/real_tool_executor.rs crates/brain-integration-tests/tests/dynamic_tools.rs
git commit -m "refactor(tools): remove fake production capabilities"
```

### Task 6: 暴露安全的工具状态与 reload API

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/api_server.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/ws_handler.rs`
- Modify: `rust/crates/ai-brain-cli/src/tool_runtime.rs`

- [ ] **Step 1: 写状态脱敏、reload 和远程策略 RED 测试**

```rust
#[tokio::test]
async fn tools_status_reports_sources_without_secrets() {
    let app = test_router(runtime_with_secret_backends()).await;
    let response = request_json(&app, Method::GET, "/api/tools/status", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await;
    assert!(body.contains("registry_version"));
    assert!(body.contains("mcp:docs"));
    for secret in ["Bearer secret", "DOCS_TOKEN_VALUE", "oauth-code"] {
        assert!(!body.contains(secret));
    }
}

#[tokio::test]
async fn reload_endpoint_changes_version_and_rejects_unknown_scope() {
    let app = test_router(runtime_fixture()).await;
    let before = get_status(&app).await.registry_version;
    mutate_skill_fixture(&app).await;
    let ok = post_json(&app, "/api/tools/reload", json!({"scope":"skills"})).await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert!(get_status(&app).await.registry_version > before);
    let unchanged = post_json(&app, "/api/tools/reload", json!({"scope":"skills"})).await;
    assert_eq!(unchanged.status(), StatusCode::OK);
    assert!(!unchanged.json().await["changed"].as_bool().unwrap());
    assert_eq!(post_json(&app, "/api/tools/reload", json!({"scope":"unknown"})).await.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn remote_tool_routes_are_behind_existing_tailscale_middleware() {
    let app = remote_test_router();
    assert_eq!(request_without_identity(&app, "/api/tools/status").await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(request_with_identity(&app, "/api/tools/status").await.status(), StatusCode::OK);
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p ai-brain-cli tools_status_api --lib -- --nocapture`

Expected: routes 尚不存在。

- [ ] **Step 3: 实现共享 router handlers**

- `GET /api/tools/status` 返回 ToolRuntimeStatus：registry_version/tool_count/providers/tools/diagnostics；tool 仅含 name/aliases/provider/source/exposure/permission/risk，provider 含 health/last_success_at/updated_at，diagnostics 只含固定 code 和脱敏摘要。
- `POST /api/tools/reload` body `{scope:"all|builtin|mcp|plugins|skills|web"}`，调用 async runtime reload，返回每 provider 结果、`changed` 和 version；未知 scope 400。只有 fingerprint 真正变化时 version 才增加，无变化 reload 返回 `changed=false`。
- Web UI Router 的 AppState 已含 `Arc<Orchestrator>`，从它取得 `Arc<ToolRuntime>`；HTTP API 的 SharedOrch 如保留 Mutex，不得在 await 外部 I/O 时持有整个 Orchestrator mutex，先 clone runtime 再释放 guard。
- 两条 route 加到本地 API 与 Web UI router；remote Web 仍在现有 Tailscale middleware layer 之内。
- 错误响应中文，内部细节经过 sanitized error。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p ai-brain-cli tools_status_api --lib -- --nocapture
cargo test -p ai-brain-cli remote_policy --lib -- --nocapture
cargo clippy -p ai-brain-cli --all-targets -- -D warnings
```

Expected: 状态、reload、脱敏、远程认证测试通过。

```powershell
git add crates/ai-brain-cli/src/api_server.rs crates/ai-brain-cli/src/tool_runtime.rs crates/ai-brain-cli/src/web/ws_handler.rs
git commit -m "feat(api): expose dynamic tool status and reload"
```

### Task 7: 建立 release 冒烟脚本并验证真实 open-websearch

**Files:**
- Create: `rust/scripts/smoke-v2-dynamic-tools.ps1`
- Modify: `rust/crates/ai-brain-cli/src/main.rs`
- Modify: `rust/crates/ai-brain-cli/src/command/mod.rs`
- Create: `rust/crates/ai-brain-cli/src/command/tools_cmd.rs`
- Modify: `rust/crates/ai-brain-cli/Cargo.toml`

- [ ] **Step 1: 给 CLI 增加只读/显式 reload 命令 RED 测试**

在 `main.rs` 增加 `Commands::Tools { action: ToolsAction }` 并委托 `command::tools_cmd::handle_tools_command`；`tools_cmd.rs` 定义 clap `ToolsAction::{Status, Reload, Call}`，新增命令：

```text
ai-brain tools status --json
ai-brain tools reload --scope all --json
ai-brain tools call --name <name> --input-json <json>
```

`tools call` 只用于本机 smoke，仍走 registry snapshot、policy、context 和 route；危险工具没有确认 channel 时拒绝。handler 返回 `Result<String, ToolCommandError>`，main 统一打印并设置非零退出；测试直接调用 handler，断言未知工具为 `NotFound`、read_file fixture 成功、MCP fixture 成功，且 JSON 输入不是 object 时为 `InvalidInput`。`--json` 输出使用可序列化 report，不混入日志或人类提示。

Run: `cargo test -p ai-brain-cli tools_command --lib -- --nocapture`

Expected: 命令尚不存在。

- [ ] **Step 2: 实现命令和可重复 smoke 脚本**

脚本参数：

```powershell
param(
  [string]$Binary = ".\target\release\ai-brain.exe",
  [string]$Listen = "127.0.0.1:18080",
  [string]$Workspace = (Get-Location).Path,
  [switch]$RequireOpenWebSearch
)
```

脚本必须：

1. 验证 Binary/Workspace 的 resolved path，创建独立临时日志目录，不修改用户配置。
2. `tools status --json`，断言每个 advertised tool 有 provider/source，生产无假工具。
3. 调 `read_file` 读取 workspace `Cargo.toml`。
4. 若用户配置含 open-websearch：找出其实际 qualified search tool，调用一个稳定查询并要求 `is_error=false`、非空结果；`-RequireOpenWebSearch` 下缺失/失败则脚本失败。
5. 启动 `web --addr $Listen` 时用 `Start-Process -WindowStyle Hidden -PassThru`，轮询 `/api/tools/status` 最多 30 秒。
6. 先修改 fixture 配置，再 POST reload，确认 `changed=true` 且 version 增长；紧接着不修改配置再次 reload，确认 `changed=false` 且 version 不变。
7. finally 中只停止脚本启动且 PID 完全匹配的进程；禁止递归删除 workspace 或用户目录。
8. 输出脱敏 JSON 摘要和日志路径。

为脚本写 Pester 不是仓库现有依赖，因此给脚本增加 `[switch]$WhatIfFixtures`；该模式创建最小临时 workspace/config、使用传入 debug binary 跑 status/read/reload，然后 finally 验证子进程退出且只删除脚本生成的临时目录。Rust CLI 测试覆盖命令 handler 行为。

- [ ] **Step 3: GREEN 和提交**

Run:

```powershell
cargo test -p ai-brain-cli tools_command --lib -- --nocapture
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-v2-dynamic-tools.ps1 -Binary .\target\debug\ai-brain.exe -WhatIfFixtures
```

Expected: 命令和 hermetic 脚本测试通过。

```powershell
git add crates/ai-brain-cli/Cargo.toml crates/ai-brain-cli/src/command/mod.rs crates/ai-brain-cli/src/command/tools_cmd.rs crates/ai-brain-cli/src/main.rs scripts/smoke-v2-dynamic-tools.ps1
git commit -m "test(v2): add dynamic tool release smoke"
```

### Task 8: 全量验证、替换运行服务并留下可审查证据

**Files:**
- Review: all files changed since `408c0488`
- Review only: `src/`
- Review only: `tests/test_porting_workspace.py`

- [ ] **Step 1: 规格覆盖与红旗扫描**

Run:

```powershell
git diff --check 408c0488..HEAD
rg -n -i 'not yet implemented|stub response|MCP tool proxy not yet connected|LSP server not connected|Testing permission tool stub' crates/brain-core crates/brain-main crates/brain-motor crates/brain-mcp crates/tools crates/plugins crates/brain-plugin crates/brain-evolver crates/ai-brain-cli
rg -n 'mvp_tool_definitions\(|register_tools\(tool_defs\)|with_builtin_tools\(|execute_stub\(|StubWebSearch' crates
rg -n 'ToolExecutionResult\s*\{[^}]*is_error:\s*false' crates/brain-mcp crates/tools crates/brain-motor crates/ai-brain-cli
```

Expected: 前两项无生产命中；第三项的多行正则若 rg 不支持则用 `rg -n 'is_error: false'` 人工审查相邻分支，确认没有错误文案假成功。测试 mock 名称可存在于测试模块，但生产构造不可引用。

- [ ] **Step 2: 完整格式、Clippy、Rust 与 Python 门禁**

Run from `rust/`:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
Set-Location ..
python -m unittest tests.test_porting_workspace
Set-Location rust
cargo build --release -p ai-brain-cli
```

Expected: 所有命令退出 0。Python `src/` 是归档兼容镜像，本任务不把其 100+ 静态工具快照冒充 Rust v2 动态 runtime；只要求既有测试无回归。

- [ ] **Step 3: 运行真实 open-websearch/reload/release smoke**

Run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-v2-dynamic-tools.ps1 `
  -Binary .\target\release\ai-brain.exe `
  -Listen 127.0.0.1:18080 `
  -Workspace (Get-Location).Path `
  -RequireOpenWebSearch
```

Expected: builtin read、用户 open-websearch MCP 调用、HTTP status/reload/version 全成功；输出没有凭据。

- [ ] **Step 4: 安全替换当前 8080 服务**

先只读确认当前监听进程：

```powershell
$connection = Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort 8080 -State Listen -ErrorAction SilentlyContinue
$connection | Select-Object LocalAddress, LocalPort, OwningProcess
if ($connection) { Get-Process -Id $connection.OwningProcess | Select-Object Id, Path, StartTime }
```

只有 Path 明确指向本仓库 ai-brain binary 时才停止该 PID；若属于其他程序，停止并报告冲突，不能杀进程。随后：

```powershell
$binary = (Resolve-Path .\target\release\ai-brain.exe).Path
$process = Start-Process -FilePath $binary -ArgumentList @('web','--addr','127.0.0.1:8080') -WorkingDirectory (Get-Location).Path -WindowStyle Hidden -PassThru
$process.Id
Invoke-RestMethod http://127.0.0.1:8080/api/tools/status | ConvertTo-Json -Depth 8
```

Expected: 新 PID 存活，status HTTP 200，registry version/tool count/provider health 合理。记录旧/新 PID 和 release binary hash：

```powershell
Get-FileHash .\target\release\ai-brain.exe -Algorithm SHA256
```

- [ ] **Step 5: 最终审查提交**

```powershell
git status --short
git log --oneline --decorate 408c0488..HEAD
git diff --stat 408c0488..HEAD
git diff --check 408c0488..HEAD
```

各子任务已经逐个提交；这里先确认没有遗留业务改动。保留用户原有 `.idea/vcs.xml`、`rust/.clawd-todos.json`、`rust/crates/brain-llm/src/openai_compat.rs` 和所有无关未跟踪内容，禁止用目录级 `git add crates`。

```powershell
git diff --cached --check
git diff --cached --name-only
```

Expected: cached diff 为空；若验证确实暴露并修复了缺口，只用列出该修复具体文件的 `git add <exact-file>...` 后创建 `fix(v2): close dynamic tool verification gap` 提交，先人工检查其中不含既有用户改动。没有新变化不创建空提交。最终交付报告必须包含：各阶段提交、新鲜测试命令与结果、release SHA-256、新服务 PID、真实 open-websearch 调用摘要、状态接口版本/工具数，以及因未配置真实后端而明确不广告的能力。
