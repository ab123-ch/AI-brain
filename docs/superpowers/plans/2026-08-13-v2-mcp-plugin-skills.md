# 智脑 v2 MCP、Plugin 与 Skills 动态刷新 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用真实 stdio/Streamable HTTP MCP 客户端、启用状态驱动的 PluginProvider 和版本化 SkillCatalogSnapshot 替换 v2 的空池与静态启动扫描，并支持显式 reload、通知刷新、故障隔离和有界关闭。

**Architecture:** `brain-mcp` 成为 canonical MCP 配置与连接边界，每个 server 独立持有 rmcp client 和工具/资源缓存，`McpProvider` 将每个 server 原子发布为独立 contribution。`plugins::PluginProvider` 从正式 PluginManager 的 enabled registry 生成限定名 route；`brain-plugin::SkillCatalogRegistry` 维护最后健康 catalog 快照。CLI 的 `ToolRuntime` 只负责按顺序刷新 Provider、聚合状态并关闭生命周期。

**Tech Stack:** Rust 2021、Tokio、rmcp 3.1.2、reqwest/rustls、serde/serde_json、现有 runtime/plugins/brain-plugin/ai-brain-cli crates。

---

## 文件结构

- Modify: `rust/crates/brain-mcp/Cargo.toml`
- Modify: `rust/crates/brain-mcp/src/lib.rs`
- Replace: `rust/crates/brain-mcp/src/config.rs`
- Replace: `rust/crates/brain-mcp/src/client_pool.rs`
- Create: `rust/crates/brain-mcp/src/client.rs`
- Create: `rust/crates/brain-mcp/src/provider.rs`
- Create: `rust/crates/brain-mcp/src/auth.rs`
- Create: `rust/crates/brain-mcp/tests/fixtures/stdio_server.py`
- Create: `rust/crates/brain-mcp/tests/stdio_transport.rs`
- Create: `rust/crates/brain-mcp/tests/http_transport.rs`
- Modify: `rust/crates/runtime/Cargo.toml`
- Modify: `rust/crates/runtime/src/config.rs`
- Modify: `rust/crates/runtime/src/mcp_stdio.rs`
- Modify: `rust/crates/runtime/src/lib.rs`
- Modify: `rust/crates/plugins/Cargo.toml`
- Modify: `rust/crates/plugins/src/lib.rs`
- Create: `rust/crates/plugins/src/provider.rs`
- Modify: `rust/crates/brain-plugin/src/lib.rs`
- Create: `rust/crates/brain-plugin/src/skill_registry.rs`
- Modify: `rust/crates/brain-plugin/src/skill_loader.rs`
- Modify: `rust/crates/ai-brain-cli/src/tool_runtime.rs`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`
- Modify: `rust/crates/ai-brain-cli/src/real_tool_executor.rs`
- Modify: `rust/crates/ai-brain-cli/src/tui/app.rs`
- Modify: `rust/crates/ai-brain-cli/src/tui/command_panel.rs`
- Modify: `rust/crates/brain-integration-tests/tests/dynamic_tools.rs`
- Modify: `rust/Cargo.lock`

## 固定依赖与生命周期常量

`brain-mcp/Cargo.toml` 必须精确使用：

```toml
rmcp = { version = "3.1.2", default-features = false, features = [
  "client",
  "transport-child-process",
  "transport-streamable-http-client-reqwest",
  "reqwest",
  "auth",
  "which-command",
] }

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = [
  "Win32_Foundation",
  "Win32_Security",
  "Win32_Security_Authorization",
  "Win32_Storage_FileSystem",
  "Win32_System_Memory",
  "Win32_System_Threading",
] }

[target.'cfg(unix)'.dev-dependencies]
libc = "0.2"
```

`rmcp 3.1.2` 的 MSRV 是 Rust 1.88；执行前先跑 `rustc --version` 并要求 `>=1.88`（当前审查环境为 1.95.0）。若 CI toolchain 更旧，先更新项目 toolchain 文件，不得悄悄降回 rmcp 1.x。

显式版本列表必须是：

```rust
pub fn preferred_protocol_versions() -> Vec<ProtocolVersion> {
    vec![
        ProtocolVersion::V_2026_07_28,
        ProtocolVersion::V_2025_11_25,
        ProtocolVersion::V_2025_06_18,
        ProtocolVersion::V_2025_03_26,
    ]
}
```

不要使用 `ProtocolVersion::LATEST`；rmcp 3.1.2 中它仍等于 `V_2025_11_25`。

### Task 1: 收敛 canonical MCP 配置与确定性 scope 合并

**Files:**
- Replace: `rust/crates/brain-mcp/src/config.rs`
- Modify: `rust/crates/brain-mcp/src/lib.rs`
- Modify: `rust/crates/runtime/src/config.rs`
- Modify: `rust/crates/runtime/src/lib.rs`
- Modify: `rust/crates/runtime/Cargo.toml`

- [ ] **Step 1: 写旧 JSON、HTTP、安全引用与 scope RED 测试**

```rust
#[test]
fn parses_legacy_stdio_and_streamable_http_without_materializing_secrets() {
    let config = McpConfigSet::parse_sources(vec![
        source(ConfigScope::User, r#"{
          "mcpServers": {
            "local": {"command":"node","args":["server.js"],"env":{"MODE":"prod"}},
            "docs": {"type":"http","url":"https://mcp.example.test","bearerTokenEnv":"DOCS_TOKEN"}
          }
        }"#),
    ]).unwrap();
    assert!(matches!(config.server("local").unwrap().transport, McpTransportConfig::Stdio(_)));
    let docs = config.server("docs").unwrap();
    assert_eq!(docs.auth, McpAuthConfig::BearerEnv("DOCS_TOKEN".into()));
    assert!(!format!("{docs:?}").contains("secret"));
}

#[test]
fn project_scope_overrides_user_and_order_is_stable() {
    let config = McpConfigSet::parse_sources(vec![user_source(), project_source()]).unwrap();
    assert_eq!(config.server("same").unwrap().scope, ConfigScope::Project);
    assert_eq!(config.server_names(), vec!["alpha", "same", "zeta"]);
}

#[test]
fn plugin_scope_is_namespaced_and_cannot_shadow_user_server() {
    let config = McpConfigSet::parse_sources(vec![user_same(), plugin_same("demo")]).unwrap();
    assert_eq!(config.server("same").unwrap().scope, ConfigScope::User);
    assert!(config.server("plugin__demo__same").is_some());
}

#[test]
fn unsupported_sse_is_diagnostic_not_a_fake_server() {
    let config = McpConfigSet::parse_sources(vec![source(
        ConfigScope::User,
        r#"{"mcpServers":{"old":{"type":"sse","url":"http://127.0.0.1/sse"}}}"#,
    )]).unwrap();
    assert!(config.server("old").is_none());
    assert_eq!(config.diagnostics()[0].code, "unsupported_transport");
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-mcp config --lib -- --nocapture`

Expected: 新配置类型不存在。

- [ ] **Step 3: 实现 canonical 配置**

核心类型固定为：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigScope { User, Project, Plugin, MachineOverride }

#[derive(Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub scope: ConfigScope,
    pub enabled: bool,
    pub transport: McpTransportConfig,
    pub auth: McpAuthConfig,
    pub allow_tools: BTreeSet<String>,
    pub deny_tools: BTreeSet<String>,
    pub initialize_timeout: Duration,
    pub call_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransportConfig {
    Stdio { command: String, args: Vec<String>, env: BTreeMap<String, String> },
    StreamableHttp { url: String, headers: BTreeMap<String, HeaderValueRef> },
}

#[derive(Clone, PartialEq, Eq)]
pub enum McpAuthConfig {
    None,
    BearerEnv(String),
    OAuth { credential_key: String, interactive: bool },
}
```

手工实现 `Debug`，BearerEnv 只显示环境变量名，OAuth 只显示 credential_key/interactivity，任何 resolved secret 均显示 `[REDACTED]`。解析优先级按 enum 顺序后写覆盖，最后用 BTreeMap 排序。支持当前 `command/args/env/type/url`，新增 `enabled`、allow/deny、timeouts、headers env 引用、bearer env 和 OAuth 引用。Plugin scope 只能覆盖该插件自有 namespace，不能替换同名 user/project server；MachineOverride 只从受信任本机配置入口创建，不接受插件 manifest 自报 scope。SSE/WS/SDK/managed proxy 进入 diagnostics，绝不返回可连接 server。

`runtime::config` 不再定义另一套 MCP struct：改为 re-export `brain_mcp::config` 的对应类型，`RuntimeConfig::mcp()` 返回 canonical `McpConfigSet`。为打破依赖环，确认 `brain-mcp` 不依赖 runtime；runtime 增加对 brain-mcp 的单向依赖。旧 parse helper 迁移到 brain-mcp，并保留 runtime 配置加载测试。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p brain-mcp config --lib -- --nocapture
cargo test -p runtime config --lib -- --nocapture
cargo clippy -p brain-mcp -p runtime --all-targets -- -D warnings
```

Expected: 配置兼容、scope、脱敏、unsupported diagnostics 测试通过。

```powershell
git add crates/brain-mcp/src/config.rs crates/brain-mcp/src/lib.rs crates/runtime/Cargo.toml crates/runtime/src/config.rs crates/runtime/src/lib.rs
git commit -m "refactor(mcp): unify scoped server configuration"
```

### Task 2: 用 rmcp 实现独立 stdio client 并迁移现有 fixture 合同

**Files:**
- Modify: `rust/crates/brain-mcp/Cargo.toml`
- Create: `rust/crates/brain-mcp/src/client.rs`
- Create: `rust/crates/brain-mcp/tests/fixtures/stdio_server.py`
- Create: `rust/crates/brain-mcp/tests/stdio_transport.rs`
- Modify: `rust/crates/runtime/src/mcp_stdio.rs`
- Modify: `rust/Cargo.lock`

- [ ] **Step 1: 将 runtime 已有 stdio fixture 合同写入 brain-mcp RED 集成测试**

fixture 必须支持 initialize、分页 tools/list、tools/call、分页 resources/list、resources/read、tools/list_changed、故意超时与退出。测试：

```rust
#[tokio::test]
async fn stdio_client_discovers_pages_calls_tools_and_reads_resources() {
    let mut client = McpServerClient::connect(fixture_stdio_config()).await.unwrap();
    let discovery = client.discover().await.unwrap();
    assert_eq!(raw_tool_names(&discovery), vec!["first", "second"]);
    assert_eq!(discovery.resources.len(), 2);
    let call = client.call_tool("second", json!({"value": 7})).await.unwrap();
    assert_eq!(call.structured_content.unwrap()["value"], 7);
    let resource = client.read_resource("fixture://two").await.unwrap();
    assert_eq!(resource.contents[0].text.as_deref(), Some("two"));
}

#[tokio::test]
async fn stdio_timeout_is_typed_and_close_reaps_the_child() {
    let mut client = McpServerClient::connect(fixture_stdio_config()).await.unwrap();
    let error = client.call_tool("hang", json!({})).await.unwrap_err();
    assert!(matches!(error.kind(), McpClientErrorKind::Timeout));
    client.close(Duration::from_secs(2)).await.unwrap();
    assert!(!client.is_running());
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-mcp --test stdio_transport -- --nocapture`

Expected: client/fixture API 尚不存在。

- [ ] **Step 3: 固定 rmcp 依赖并实现 client trait**

实现 `McpConnection` 对象安全 trait：

```rust
pub trait McpConnection: Send + Sync {
    fn server_name(&self) -> &str;
    fn protocol_version(&self) -> ProtocolVersion;
    fn discover(&self) -> McpFuture<'_, Result<McpDiscovery, McpClientError>>;
    fn call_tool(&self, raw_name: &str, input: Value)
        -> McpFuture<'_, Result<CallToolResult, McpClientError>>;
    fn list_resources(&self) -> McpFuture<'_, Result<Vec<Resource>, McpClientError>>;
    fn read_resource(&self, uri: &str)
        -> McpFuture<'_, Result<ReadResourceResult, McpClientError>>;
    fn close(&self, timeout: Duration) -> McpFuture<'_, Result<(), McpClientError>>;
}
```

同文件定义 `pub type McpFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;`；定义 `McpClientErrorKind { InvalidConfig, Authentication, Timeout, Transport, Protocol, Backend, Closed }`。`McpClientError` 保存 kind、sanitized message 与可选 server/method；`Display/Debug` 不含 header、token 或请求 body。映射固定为：配置/secret 缺失→InvalidConfig，401/403/OAuth→Authentication，外层 deadline→Timeout，HTTP/stdio/进程断开→Transport，JSON-RPC/schema/分页 cursor→Protocol，MCP error result/`is_error=true`→Backend，已关闭 generation→Closed。

stdio 建连必须用 `rmcp::transport::which_command` 先解析配置 executable（Windows 的 `.cmd/.exe` shim 也由此处理），在返回的 `tokio::process::Command` 上注入 args/env，再交给 `rmcp::transport::TokioChildProcess`，然后：

```rust
handler.serve_with_lifecycle(
    transport,
    ClientLifecycleMode::Auto {
        preferred_versions: preferred_protocol_versions(),
        legacy_version: Some(ProtocolVersion::V_2025_03_26),
    },
).await
```

使用 `RunningService::list_all_tools/list_all_resources/call_tool/read_resource`；每项外层 `tokio::time::timeout` 使用 config 值。`CallToolResult.is_error == true` 映射为 Backend error，不伪装成功。`McpServerClient` 内部用 `tokio::sync::Mutex<Option<RunningService<...>>>` 满足 `RunningService::close(&mut self)`；`close` 优先调用 rmcp 3.1.2 的 `close_with_timeout`（或外层 timeout 包 `close()`），关闭/超时时取消 service，让 transport drop cleanup kill 并 wait 子进程。fixture 记录 child PID，测试在 deadline 后轮询确认 PID 已退出，不能只信 `is_running()` 内存标志。

现有 `runtime::McpServerManager` 测试先保持；把它改成包裹 brain-mcp client 的兼容 facade，保留 public response adapter，删除其自研进程/JSON-RPC 生产实现后再运行所有 runtime MCP 测试。若某个旧测试只检查内部 request id，则迁移为对外行为测试，不复制第二套协议客户端。

- [ ] **Step 4: GREEN、依赖特性检查和提交**

Run:

```powershell
cargo tree -p brain-mcp -e features | Select-String 'rmcp v3.1.2|transport-child-process|transport-streamable-http-client-reqwest|which-command'
cargo test -p brain-mcp --test stdio_transport -- --nocapture
cargo test -p runtime mcp_stdio --lib -- --nocapture
cargo clippy -p brain-mcp -p runtime --all-targets -- -D warnings
```

Expected: tree 包含 rmcp 3.1.2 与目标 transport；stdio 全合同通过。

```powershell
git add crates/brain-mcp/Cargo.toml crates/brain-mcp/src/client.rs crates/brain-mcp/tests/fixtures/stdio_server.py crates/brain-mcp/tests/stdio_transport.rs crates/runtime/src/mcp_stdio.rs Cargo.lock
git commit -m "feat(mcp): connect real stdio servers with rmcp"
```

### Task 3: 实现 Streamable HTTP 的 2025 与 2026-07-28 生命周期

**Files:**
- Modify: `rust/crates/brain-mcp/src/client.rs`
- Create: `rust/crates/brain-mcp/tests/http_transport.rs`

- [ ] **Step 1: 写本地 HTTP fixture RED 测试**

在测试内用 axum 启动 localhost server，记录 method/header/body。覆盖两套 server：

```rust
#[tokio::test]
async fn legacy_http_initializes_session_and_propagates_protocol_header() {
    let server = LegacyHttpFixture::start().await;
    let client = McpServerClient::connect(server.config()).await.unwrap();
    client.discover().await.unwrap();
    let requests = server.requests().await;
    assert_eq!(requests[0].method, "initialize");
    assert!(requests.iter().skip(1).all(|r| r.session_id.as_deref() == Some("fixture-session")));
    assert!(requests.iter().all(|r| r.protocol_version.is_some()));
}

#[tokio::test]
async fn draft_http_uses_discover_without_session_and_standard_headers() {
    let server = DraftHttpFixture::start().await;
    let client = McpServerClient::connect(server.config()).await.unwrap();
    client.call_tool("echo", json!({"x":1})).await.unwrap();
    let requests = server.requests().await;
    assert_eq!(requests[0].method, "server/discover");
    assert!(requests.iter().all(|r| r.session_id.is_none()));
    let call = requests.iter().find(|r| r.method == "tools/call").unwrap();
    assert_eq!(call.mcp_method.as_deref(), Some("tools/call"));
    assert_eq!(call.mcp_name.as_deref(), Some("echo"));
}

#[tokio::test]
async fn http_supports_json_and_sse_and_rejects_non_success_status() {
    let fixture = MixedResponseFixture::start().await;
    assert!(fixture.client().call_tool("json", json!({})).await.is_ok());
    assert!(fixture.client().call_tool("sse", json!({})).await.is_ok());
    let error = fixture.client().call_tool("fail", json!({})).await.unwrap_err();
    assert!(matches!(error.kind(), McpClientErrorKind::Transport));
}
```

测试显式给 reqwest client 设置 `no_proxy()` 或 `NO_PROXY=127.0.0.1,localhost` 的串行 guard，避免本机 7890 代理污染。

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-mcp --test http_transport -- --nocapture`

Expected: HTTP transport 未实现。

- [ ] **Step 3: 用 rmcp Streamable HTTP 实现连接**

- 创建 reqwest client，设置 connect/request timeout、有限 redirect policy、系统 proxy 行为与 resolved headers。endpoint 先走共享 RemoteUrl 语法/IP guard，DNS 每个地址和每跳 redirect 再拒绝 loopback/link-local/私网；只有测试构造器可显式允许 fixture loopback。远程 MCP 配置不能借 DNS rebinding 绕过网络策略。
- 构造 `StreamableHttpClientTransportConfig::with_uri(url)`，设置 custom_headers、auth_header、allow_stateless=true、reinit_on_expired_session=true 和 SSE event size 上限。
- 使用与 stdio 相同的 `ClientLifecycleMode::Auto { preferred_versions, legacy_version }`；禁止分叉手写 lifecycle。
- rmcp 根据协商协议产生 session 与 SEP-2243 标准 header；fixture 对 wire 行为断言。
- 非 2xx、unexpected content type、SSE 解析、session expired、timeout 映射到 typed error。
- resolved bearer/header secret 只存在于 transport config，不进入 Debug/ToolDescriptor/status。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p brain-mcp --test http_transport -- --nocapture
cargo test -p brain-mcp --test stdio_transport -- --nocapture
cargo clippy -p brain-mcp --all-targets -- -D warnings
```

Expected: 2025 session、2026 stateless、JSON/SSE/non-2xx 全部通过。

```powershell
git add crates/brain-mcp/src/client.rs crates/brain-mcp/tests/http_transport.rs
git commit -m "feat(mcp): support streamable http lifecycles"
```

### Task 4: 实现 per-server pool、名称映射、资源与故障隔离

**Files:**
- Replace: `rust/crates/brain-mcp/src/client_pool.rs`
- Modify: `rust/crates/brain-mcp/src/lib.rs`
- Modify: `rust/crates/brain-mcp/tests/stdio_transport.rs`
- Modify: `rust/crates/brain-mcp/tests/http_transport.rs`

- [ ] **Step 1: 写 pool 隔离与编解码 RED 测试**

```rust
#[test]
fn qualified_name_round_trips_raw_segments_with_separators() {
    let names = McpNameMap::build(vec![
        ("Docs Server", "find/docs"),
        ("Docs-Server", "find docs"),
    ]).unwrap();
    for route in names.routes() {
        assert_eq!(names.resolve(&route.qualified).unwrap().raw_tool, route.raw_tool);
    }
    assert_eq!(names.routes().iter().map(|r| &r.qualified).collect::<BTreeSet<_>>().len(), 2);
}

#[tokio::test]
async fn one_bad_server_does_not_remove_a_healthy_server() {
    let pool = McpClientPool::connect_all(configs_with_good_and_bad()).await;
    assert!(pool.server_status("good").await.is_connected());
    assert!(pool.server_status("bad").await.is_error());
    assert!(pool.tool("mcp__good__echo").await.is_some());
}

#[tokio::test]
async fn disconnect_removes_only_that_servers_routes_and_reconnect_restores_them() {
    let pool = pool_with_two_servers().await;
    pool.disconnect("first").await.unwrap();
    assert!(pool.tool("mcp__first__echo").await.is_none());
    assert!(pool.tool("mcp__second__echo").await.is_some());
    pool.reconnect("first").await.unwrap();
    assert!(pool.tool("mcp__first__echo").await.is_some());
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-mcp client_pool --lib -- --nocapture`

Expected: stub pool 无真实连接/索引。

- [ ] **Step 3: 实现连接池与不可变 server snapshot**

核心字段：

```rust
pub struct McpClientPool {
    servers: tokio::sync::RwLock<BTreeMap<String, Arc<McpServerHandle>>>,
    revision_tx: tokio::sync::watch::Sender<u64>,
}

pub struct McpServerSnapshot {
    pub name: String,
    pub status: ServerStatus,
    pub protocol_version: Option<String>,
    pub tools: Vec<McpToolRecord>,
    pub resources: Vec<McpResourceRecord>,
    pub last_error: Option<SanitizedError>,
}
```

规则：

- `connect_all` 总是返回 pool + 每服务状态，不因单服务失败返回全局 Err。
- 每个 server handle 用自己的 async mutex 串行执行 rmcp RunningService 可变操作；不同 server 并行。
- name normalization 统一调用子计划 A 的 `qualify_tool_name("mcp", &[raw_server, raw_tool])`；空 segment 报错，超过 64 字符或归一化碰撞时调用 `disambiguate_tool_name(base, raw_identity)` 追加稳定 8 位 SHA-256 后缀。raw route 存 map，禁止靠拆 qualified name 反推 raw 名，也禁止各 Provider 自创不一致的截断算法。
- allow/deny 在 discovery 后、发布前应用；deny 优先。
- pool API 包含 `call_qualified`、`list_all_resources`、`read_resource(server, uri)`、`disconnect/reconnect/shutdown`、`snapshots`、`subscribe_revision`。
- tool/resource cache 只有完整分页成功后才替换；失败保留 last good 并将 status 标 degraded，已确认断线/禁用则清空该 server 路由。
- `McpToolRoute` 不在执行时用 qualified name 回查“当前”pool。每次完整 discovery 生成不可变 `McpServerSnapshot`，其中每条 `McpToolRecord` 持有当代 `Arc<dyn McpServerClient>`、raw tool name 与 schema；Provider route clone 这三个值。重连发布新 contribution 后，旧请求仍持有旧 client Arc；pool 把旧 generation 标 draining，等 `Arc::strong_count`/in-flight guard 归零或 shutdown deadline 到达再 close。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p brain-mcp client_pool --lib -- --nocapture
cargo test -p brain-mcp --test stdio_transport -- --nocapture
cargo test -p brain-mcp --test http_transport -- --nocapture
```

Expected: 故障隔离、名称往返、资源和重连测试通过。

```powershell
git add crates/brain-mcp/src/client_pool.rs crates/brain-mcp/src/lib.rs crates/brain-mcp/tests/stdio_transport.rs crates/brain-mcp/tests/http_transport.rs
git commit -m "feat(mcp): isolate servers in a real client pool"
```

### Task 5: 将两代工具变化机制接到原子 refresh

**Files:**
- Modify: `rust/crates/brain-mcp/src/client.rs`
- Modify: `rust/crates/brain-mcp/src/client_pool.rs`
- Modify: `rust/crates/brain-mcp/tests/stdio_transport.rs`
- Modify: `rust/crates/brain-mcp/tests/http_transport.rs`

- [ ] **Step 1: 写旧 notification 与新 subscription RED 测试**

```rust
#[tokio::test]
async fn legacy_list_changed_refreshes_all_pages_once() {
    let fixture = legacy_change_fixture().await;
    let mut revisions = fixture.pool.subscribe_revision();
    fixture.server.publish_tool_list_changed(vec!["new-a", "new-b"]).await;
    revisions.changed().await.unwrap();
    assert_eq!(fixture.pool.server_snapshot("legacy").await.tool_names(), vec!["new-a", "new-b"]);
    assert_eq!(fixture.server.list_calls_after_notification(), 2);
}

#[tokio::test]
async fn draft_subscription_refreshes_and_resubscribes_after_stream_end() {
    let fixture = draft_subscription_fixture().await;
    fixture.server.publish_subscription_change("tools").await;
    fixture.wait_for_tool("mcp__draft__new").await;
    fixture.server.end_subscription().await;
    fixture.server.publish_subscription_change("tools").await;
    fixture.wait_for_revision(3).await;
    assert!(fixture.server.listen_call_count() >= 2);
}
```

- [ ] **Step 2: 运行 RED**

Run:

```powershell
cargo test -p brain-mcp list_changed -- --nocapture
cargo test -p brain-mcp subscription -- --nocapture
```

Expected: revision 不变化。

- [ ] **Step 3: 实现 bounded refresh channel**

- `AiBrainClientHandler::on_tool_list_changed` 和 `on_resource_list_changed` 只 `try_send(McpChange)` 到容量 1 的 channel，禁止在 rmcp callback 内做 I/O；满 channel 代表已有刷新待处理。
- legacy 版本使用 handler 通知。
- 2026-07-28 使用 rmcp 3.1.2 的 `Peer::listen(SubscriptionFilter)`，保存返回的 `Subscription` 并循环 `Subscription::next()`；只接受 acknowledged filter 内、subscription id 匹配的 tools/resources 通知。`end()` 为 Abrupt/Lagged 或 `next()` 出错时指数退避 250ms→5s 重订阅；成功重订阅后先完整 discover，Cancelled/主动 shutdown 不重订阅。
- refresh worker debounce 50ms，执行完整 `list_all_tools/list_all_resources`，成功后一次替换 server snapshot 并 revision+1。
- worker 结束时通过 cancellation token 停止；shutdown 等待 join handle，不能泄漏重连任务。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p brain-mcp list_changed -- --nocapture
cargo test -p brain-mcp subscription -- --nocapture
cargo test -p brain-mcp --all-targets
```

Expected: 两代更新、合并通知、断流重订阅和 shutdown 通过。

```powershell
git add crates/brain-mcp/src/client.rs crates/brain-mcp/src/client_pool.rs crates/brain-mcp/tests/stdio_transport.rs crates/brain-mcp/tests/http_transport.rs
git commit -m "feat(mcp): refresh tools from protocol notifications"
```

### Task 6: 实现 `McpProvider`、资源兼容工具与真实认证边界

**Files:**
- Modify: `rust/crates/brain-mcp/Cargo.toml`
- Create: `rust/crates/brain-mcp/src/provider.rs`
- Create: `rust/crates/brain-mcp/src/auth.rs`
- Modify: `rust/crates/brain-mcp/src/lib.rs`
- Modify: `rust/crates/ai-brain-cli/src/real_tool_executor.rs`
- Modify: `rust/Cargo.lock`

- [ ] **Step 1: 写注册、资源条件和认证 RED 测试**

```rust
#[tokio::test]
async fn provider_registers_qualified_tool_with_real_pool_route() {
    let pool = fixture_pool_with_echo().await;
    let contribution = McpProvider::new("docs", pool.clone()).discover().await.unwrap();
    let registration = find(&contribution, "mcp__docs__echo");
    let result = registration.route.execute(&call("mcp__docs__echo", json!({"x":1})), &context()).await.unwrap();
    assert!(result.output.contains("1"));
}

#[tokio::test]
async fn pinned_mcp_route_finishes_on_old_generation_after_reconnect() {
    let fixture = reconnecting_fixture_pool().await;
    let old = McpProvider::new("docs", fixture.pool.clone()).discover().await.unwrap();
    let old_route = find(&old, "mcp__docs__echo").route.clone();
    fixture.reconnect_to_generation("v2").await;
    let new = McpProvider::new("docs", fixture.pool.clone()).discover().await.unwrap();
    assert_eq!(old_route.execute(&call("mcp__docs__echo", json!({})), &context()).await.unwrap().output, "v1");
    assert_eq!(find(&new, "mcp__docs__echo").route.execute(&call("mcp__docs__echo", json!({})), &context()).await.unwrap().output, "v2");
}

#[tokio::test]
async fn resource_facades_exist_only_when_a_connected_server_has_resources() {
    assert!(!names(&provider_without_resources().discover().await.unwrap()).contains("ListMcpResources"));
    let contribution = provider_with_resources().discover().await.unwrap();
    assert!(names(&contribution).contains("ListMcpResources"));
    assert!(names(&contribution).contains("ReadMcpResource"));
}

#[test]
fn auth_tool_is_not_registered_for_static_bearer_or_noninteractive_oauth() {
    assert!(!mcp_auth_available(&McpAuthConfig::BearerEnv("TOKEN".into()), false));
    assert!(!mcp_auth_available(&oauth_config(false), true));
    assert!(mcp_auth_available(&oauth_config(true), true));
}

#[cfg(windows)]
#[tokio::test]
async fn credential_file_is_atomic_and_acl_rejects_broad_readers() {
    let store = credential_store_fixture();
    store.save(credentials_fixture()).await.unwrap();
    assert_eq!(store.load().await.unwrap().unwrap().client_id, "fixture-client");
    assert!(windows_acl_is_current_user_only(store.path()).unwrap());
    assert_no_plaintext_temp_sibling(store.path());
}

#[cfg(unix)]
#[tokio::test]
async fn credential_file_is_atomic_owned_and_mode_0600() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let store = credential_store_fixture();
    store.save(credentials_fixture()).await.unwrap();
    let metadata = std::fs::metadata(store.path()).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_no_plaintext_temp_sibling(store.path());
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-mcp provider --lib -- --nocapture`

Expected: Provider 不存在。

- [ ] **Step 3: 实现 Provider 和 routes**

- 每个 server 是 provider id `mcp:<normalized-server>`，这样一个 server 更新不会重建其他 server contribution；每次完整 discovery 的 contribution 必须 `.with_generation(server_snapshot.generation_id())`，连接代次或目录 revision 改变即使 schema 相同也替换 route。
- 每个 tool descriptor 使用 rmcp Tool 的 raw description/input_schema，canonical qualified name，`source_instance` 保存 raw server/tool；annotations 只能提升风险，不能降低默认外部工具的 `DangerFullAccess + requires_confirmation=true`。
- `McpToolRoute` 直接调用 discovery 时固定的 server generation/raw route；错误映射到 ToolRouteError 分类，禁止执行时回查 pool 最新连接。
- 资源门面属于单独 provider `mcp:resources`；input Schema 明确 server/uri，route 调用真实分页/list/read。
- 不发布通用 `MCP` 门面。
- `McpAuth` 仅当至少一个 interactive OAuth 服务待授权且 CLI/TUI 注入 `McpAuthUi` 时发布。`AiBrainCredentialStore` 实现 rmcp `CredentialStore::load/save/clear`，每个 server/credential_key 使用独立文件 `paths.credentials_root()/mcp/<safe-key>.json`。
- 凭据写入是同目录临时文件 + flush/sync + 原子 replace；Unix 用 `OpenOptionsExt::mode(0o600)` 创建并通过 uid/mode 验证 owner，测试依赖 `libc` 仅放 dev-dependencies。Windows 使用本节固定的 `windows-sys` features：获取当前进程 token/SID，构造只允许当前用户和 SYSTEM 的 protected DACL，通过 `SetNamedSecurityInfoW` 写入，再用 `GetNamedSecurityInfoW` 读取并逐项验证；所有 `LocalAlloc` 返回通过 `LocalFree` 回收。无法设置或验证安全权限时拒绝持久化并回退明确的 in-memory/重新授权状态，不能写普通明文文件继续运行。临时文件无论成功/失败都清理；状态/Debug 永不序列化 token/code，测试日志扫描不得出现 fixture secret。
- 外部 MCP metadata 默认 `DangerFullAccess + High + requires_confirmation=true`。只有 annotations 明确 `readOnlyHint=true`、`destructiveHint!=true` 且本地 server 配置没有设置更高最低权限时，才可降为 `ReadOnly`；annotations 永远不能降低本地配置的风险下限。远程 MCP 的 transport endpoint 在配置解析、DNS 解析及每次 redirect 层执行共享 URL/IP guard；普通 MCP 工具的 `guard_profile` 只按其真实输入语义设置，不能因为连接本身走 HTTP 就把任意工具参数误当 URL。所有 MCP route 仍进入共享权限/确认策略。

- [ ] **Step 4: GREEN、敏感信息扫描和提交**

Run:

```powershell
cargo test -p brain-mcp provider --lib -- --nocapture
cargo test -p brain-mcp auth --lib -- --nocapture
cargo clippy -p brain-mcp --all-targets -- -D warnings
rg -n 'Authorization|access_token|refresh_token|oauth.*code' crates/brain-mcp/src/provider.rs crates/brain-mcp/src/auth.rs
```

Expected: 测试通过；`rg` 命中仅为字段处理/脱敏测试，不存在 tracing/Display 输出秘密。

```powershell
git add crates/brain-mcp/Cargo.toml crates/brain-mcp/src/auth.rs crates/brain-mcp/src/lib.rs crates/brain-mcp/src/provider.rs crates/ai-brain-cli/src/real_tool_executor.rs Cargo.lock
git commit -m "feat(mcp): inject discovered tools and resources"
```

### Task 7: 实现正式 `PluginProvider` 与限定名/alias 策略

**Files:**
- Create: `rust/crates/plugins/src/provider.rs`
- Modify: `rust/crates/plugins/src/lib.rs`
- Modify: `rust/crates/plugins/Cargo.toml`

- [ ] **Step 1: 写启停、命名、权限和执行 RED 测试**

```rust
#[tokio::test]
async fn enabled_plugin_publishes_qualified_tool_and_disable_removes_it() {
    let fixture = plugin_fixture_with_echo();
    let provider = PluginProvider::new(fixture.manager(), PluginAliasConfig::default());
    let enabled = provider.discover().await.unwrap();
    assert!(names(&enabled).contains("plugin__demo__echo"));
    fixture.manager().lock().unwrap().disable("demo@external").unwrap();
    let disabled = provider.discover().await.unwrap();
    assert!(!names(&disabled).contains("plugin__demo__echo"));
}

#[tokio::test]
async fn plugin_route_executes_in_spawn_blocking_and_preserves_permission() {
    let registration = discovered_echo_registration(PluginToolPermission::WorkspaceWrite).await;
    assert_eq!(registration.metadata.permission, ToolPermission::WorkspaceWrite);
    assert_eq!(registration.route.execute(&call_with_text("hello"), &context()).await.unwrap().output, "hello");
}

#[tokio::test]
async fn unchanged_reload_does_not_rerun_init_and_disable_runs_shutdown_once() {
    let fixture = lifecycle_counting_plugin_fixture();
    let provider = PluginProvider::new(fixture.manager(), PluginAliasConfig::default());
    provider.discover().await.unwrap();
    provider.discover().await.unwrap();
    assert_eq!(fixture.init_calls(), 1);
    fixture.disable("demo@external");
    provider.discover().await.unwrap();
    assert_eq!(fixture.shutdown_calls(), 1);
}

#[test]
fn raw_alias_requires_explicit_unique_binding() {
    let config = PluginAliasConfig::from_pairs([("echo", "demo@external")]);
    assert_eq!(aliases_for("demo@external", "echo", &config).unwrap(), vec!["echo"]);
    assert!(aliases_for_two_plugins_with_same_raw_name(&config).is_err());
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p plugins provider --lib -- --nocapture`

Expected: provider 模块不存在。

- [ ] **Step 3: 实现 PluginProvider**

- 接受 `Arc<std::sync::Mutex<plugins::PluginManager>>`，并持有 `Mutex<BTreeMap<plugin_id, PluginGeneration>>`；discover 在 `spawn_blocking` 中执行 `plugin_registry_report`，报告 failures 进入 Provider health/diagnostics。
- 对每个 enabled plugin 以 id + version + manifest/lifecycle/tool fingerprint 标识 generation，并写入 `ProviderContribution::with_generation`：新 generation 先 validate/init，成功后才发布；fingerprint 未变不重复 init；禁用/替换的旧 generation 在其 route Arc 不再被快照持有后逆序 shutdown。初始化失败保留上一健康 contribution，且不能 shutdown 仍被旧请求使用的 generation。
- canonical 统一调用子计划 A 的 `qualify_tool_name("plugin", &[plugin_name, raw_tool])`；碰撞按 plugin id/raw identity 调用 `disambiguate_tool_name`，保持 LLM 安全字符集和 64 字符上限。
- raw name 默认不是 alias；只有 `PluginAliasConfig` 明确唯一绑定才加入。
- permission 映射严格为 read-only/workspace-write/danger-full-access；外部 execute 默认 requires_confirmation=true，read-only 可 false。
- `PluginToolRoute::execute` clone tool/input 后使用 `tokio::task::spawn_blocking(move || tool.execute(&input))`，command 非零退出映射 Backend error。
- `shutdown` 在 spawn_blocking 中对已初始化 generation 逆序且每代仅一次调用 shutdown，并聚合错误继续关闭。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p plugins provider --lib -- --nocapture
cargo test -p plugins --all-targets
cargo clippy -p plugins --all-targets -- -D warnings
```

Expected: 启停、alias、执行失败、permission、lifecycle 顺序通过。

```powershell
git add crates/plugins/Cargo.toml crates/plugins/src/lib.rs crates/plugins/src/provider.rs
git commit -m "feat(plugins): publish enabled tools dynamically"
```

### Task 8: 迁移旧插件目录并建立版本化 Skill catalog

**Files:**
- Modify: `rust/crates/brain-plugin/src/lib.rs`
- Modify: `rust/crates/brain-plugin/src/plugin_manager.rs`
- Modify: `rust/crates/brain-plugin/src/skill_loader.rs`
- Create: `rust/crates/brain-plugin/src/skill_registry.rs`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

- [ ] **Step 1: 写 legacy discovery 与 catalog 保留 RED 测试**

```rust
#[test]
fn legacy_registry_entries_become_external_plugin_directories() {
    let legacy = legacy_plugin_registry_fixture();
    let migration = LegacyPluginMigration::inspect(legacy.root()).unwrap();
    assert_eq!(migration.external_dirs(), vec![legacy.installed_plugin_dir()]);
    assert!(migration.diagnostics().is_empty());
}

#[test]
fn failed_skill_refresh_keeps_last_healthy_snapshot() {
    let registry = SkillCatalogRegistry::new();
    let first = registry.refresh(&[valid_skill_root()]).unwrap();
    corrupt_skill_root();
    let error = registry.refresh(&[corrupt_skill_root_path()]).unwrap_err();
    assert!(error.to_string().contains("SKILL.md"));
    assert_eq!(registry.snapshot().version(), first.version());
    assert_eq!(registry.snapshot().catalog().skills.len(), 1);
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-plugin skill_registry --lib -- --nocapture`

Expected: registry/migration 尚不存在。

- [ ] **Step 3: 实现迁移兼容层和 catalog snapshot**

- `LegacyPluginMigration::inspect` 只读旧 `registry.json` 和 cache 目录，输出 full PluginManager 的 `external_dirs`/diagnostics；不复制、不删除用户目录。
- full plugin loader 已支持 root `plugin.json` 和 `.claude-plugin/plugin.json`，迁移层直接传目录。
- `SkillCatalogSnapshot { version, catalog: Arc<SkillCatalog>, roots, refreshed_at }` 不可变。
- `SkillCatalogRegistry` 用 `RwLock<Arc<...>>`；扫描在锁外，成功后 version+1 原子交换，失败返回 error 且保留旧 snapshot。
- 新增 `SkillCatalog::scan_all_strict` 返回 `SkillScanReport { catalog, diagnostics }`：根目录读取失败、存在但无法读取/解析的 `SKILL.md` 都是 error；缺失的可选 root 仍可跳过。`SkillCatalogRegistry::refresh` 只接受无 error diagnostics 的完整 catalog，因此上面的 corrupt fixture 必须得到 Err，而不是被 `parse_skill_file` 静默忽略。旧宽松 `scan_all` 只保留兼容调用并标 deprecated。
- roots 固定顺序：project skills、user skills、`.codex/skills`、`.claude/skills`、enabled plugin skills；路径 canonical 去重。
- Skill tool registration 和 `SkillsSummary` prompt fragment 必须由同一个 catalog snapshot 构建；该 contribution 使用 catalog snapshot version/content hash 作为 generation，catalog 空则不注册 Skill，summary 空。
- Orchestrator 字段从 `Arc<SkillCatalog>` 改为 `Arc<SkillCatalogRegistry>`；每次请求的 provider discovery 固定一个 catalog snapshot。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p brain-plugin skill_registry --lib -- --nocapture
cargo test -p brain-plugin plugin_manager --lib -- --nocapture
cargo test -p ai-brain-cli skill --lib -- --nocapture
cargo clippy -p brain-plugin -p ai-brain-cli --all-targets -- -D warnings
```

Expected: legacy 兼容、目录优先级、刷新与 last-good 测试通过。

```powershell
git add crates/brain-plugin/src/lib.rs crates/brain-plugin/src/plugin_manager.rs crates/brain-plugin/src/skill_loader.rs crates/brain-plugin/src/skill_registry.rs crates/ai-brain-cli/src/orchestrator.rs
git commit -m "feat(skills): refresh immutable catalog snapshots"
```

### Task 9: 扩展 `ToolRuntime` 的 reload/status/shutdown 并接入 TUI

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/tool_runtime.rs`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`
- Modify: `rust/crates/ai-brain-cli/src/tui/app.rs`
- Modify: `rust/crates/ai-brain-cli/src/tui/command_panel.rs`
- Modify: `rust/crates/ai-brain-cli/Cargo.toml`

- [ ] **Step 1: 写刷新原子性和关闭顺序 RED 测试**

```rust
#[tokio::test]
async fn reload_failure_preserves_other_and_last_good_providers() {
    let fixture = RuntimeReloadFixture::started().await;
    let before = fixture.runtime.snapshot();
    fixture.break_mcp_config();
    let report = fixture.runtime.reload(ReloadScope::All).await;
    assert!(report.provider("mcp").unwrap().is_error());
    let after = fixture.runtime.snapshot();
    assert!(after.resolve("plugin__demo__echo").is_some());
    assert!(after.resolve("mcp__docs__search").is_some());
    assert!(after.version() > before.version());
}

#[tokio::test]
async fn unchanged_reload_is_successful_without_fabricating_a_new_version() {
    let fixture = RuntimeReloadFixture::started().await;
    let before = fixture.runtime.snapshot().version();
    let report = fixture.runtime.reload(ReloadScope::All).await;
    assert!(report.is_success());
    assert!(!report.changed());
    assert_eq!(fixture.runtime.snapshot().version(), before);
}

#[tokio::test]
async fn shutdown_order_is_session_plugin_mcp_builtin_and_is_bounded() {
    let fixture = RuntimeWithRecordingProviders::new();
    fixture.runtime.shutdown(Duration::from_secs(2)).await.unwrap();
    assert_eq!(fixture.events(), vec!["session", "plugin", "mcp", "builtin"]);
}
```

`break_mcp_config` 必须先把 provider 状态从 Ready 改为 Degraded，因而 state/message fingerprint 变化并递增 version；若它原本已经是相同 state/message 的重复故障报告，则不再追加重复 diagnostic，报告 `changed=false` 且 version 不变。

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p ai-brain-cli tool_runtime_reload --lib -- --nocapture`

Expected: reload/status/shutdown API 未完成。

- [ ] **Step 3: 实现生命周期 orchestration**

`ToolRuntime` 增加：

```rust
pub enum ReloadScope { All, Builtin, Mcp, Plugins, Skills, Web }

#[derive(Debug, Clone, Serialize)]
pub struct ProviderReloadResult {
    pub provider_id: String,
    pub state: ProviderState,
    pub changed: bool,
    pub error: Option<String>,
}

impl ProviderReloadResult {
    pub fn is_error(&self) -> bool { self.error.is_some() }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolRuntimeReport {
    pub before_version: u64,
    pub after_version: u64,
    pub changed: bool,
    pub providers: Vec<ProviderReloadResult>,
}

impl ToolRuntimeReport {
    pub fn changed(&self) -> bool { self.changed }
    pub fn is_success(&self) -> bool {
        self.providers.iter().all(|item| item.error.is_none())
    }
    pub fn provider(&self, id: &str) -> Option<&ProviderReloadResult> {
        self.providers.iter().find(|item| item.provider_id == id)
    }
    #[cfg(test)]
    pub fn assert_success(&self) {
        assert!(self.is_success(), "reload 失败: {:?}", self.providers);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatusView {
    pub id: String,
    pub state: ProviderState,
    pub tool_count: usize,
    pub error: Option<String>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolStatusView {
    pub name: String,
    pub aliases: Vec<String>,
    pub provider_id: String,
    pub source_kind: ToolSourceKind,
    pub exposure: ToolExposure,
    pub permission: ToolPermission,
    pub risk_level: ToolRiskLevel,
}

pub struct ToolRuntimeStatus {
    pub registry_version: u64,
    pub tool_count: usize,
    pub providers: Vec<ProviderStatusView>,
    pub tools: Vec<ToolStatusView>,
    pub diagnostics: Vec<ToolDiagnostic>,
}

impl ToolRuntime {
    pub async fn start(&self) -> ToolRuntimeReport;
    pub async fn reload(&self, scope: ReloadScope) -> ToolRuntimeReport;
    pub fn status(&self) -> ToolRuntimeStatus;
    pub async fn shutdown(&self, timeout: Duration) -> Result<(), ToolRuntimeError>;
}
```

start/reload 流程：解析配置和扫描在 registry 外；MCP services 可并发连接但逐服务 publish；plugins initialize 后 publish；skills 成功扫描后 refresh builtin Skill registration/prompt。配置 fingerprint 不变时跳过重连。状态序列化只含来源、版本、工具数、时间、sanitized error/diagnostics；只有 health state/message、generation、工具目录等语义变化真正提交时才更新时间，完全相同的重复结果保留旧时间且 no-op。诊断使用固定 code，不输出 secret、完整 header 或未经脱敏的命令参数。

Orchestrator 保存 `Arc<ToolRuntime>`，删除独立 `plugin_mgr/skill_catalog/mcp_pool` 真相字段；兼容 getter 委托 runtime。`create_v2_main_brain` 变 async 或并入 Orchestrator::new async，不再返回四元组。

TUI：

- `:mcp` 显示 runtime status 中 server 状态/工具数。
- `:mcp reload [name]` 调真实 ReloadScope::Mcp。
- plugin list/enable/disable/install/uninstall 调 full PluginManager 后 `reload(Plugins)`，删除“启动前手动编辑”提示。
- 所有 handler 保持 async；删除 runtime 内 `block_on`。

- [ ] **Step 4: 实现正常 async shutdown，Drop 只发取消**

所有 main/repl/tui/web 正常退出路径调用 `orchestrator.shutdown().await`。`Drop` 不能等待 runtime，只触发 cancellation token 并记录未显式关闭的 debug 日志。MCP close 超时后取消；plugin shutdown 失败继续关闭后续 provider并聚合错误。

- [ ] **Step 5: GREEN 和提交**

Run:

```powershell
cargo test -p ai-brain-cli tool_runtime_reload --lib -- --nocapture
cargo test -p ai-brain-cli mcp --lib -- --nocapture
cargo test -p ai-brain-cli plugin --lib -- --nocapture
cargo test -p ai-brain-cli tui --lib -- --nocapture
cargo clippy -p ai-brain-cli --all-targets -- -D warnings
```

Expected: reload、命令、状态、关闭测试通过；TUI 不再显示手工编辑占位文案。

```powershell
git add crates/ai-brain-cli/Cargo.toml crates/ai-brain-cli/src/orchestrator.rs crates/ai-brain-cli/src/tool_runtime.rs crates/ai-brain-cli/src/tui/app.rs crates/ai-brain-cli/src/tui/command_panel.rs
git commit -m "feat(cli): manage dynamic tool runtime lifecycle"
```

### Task 10: 跨 crate 热刷新门禁与当前用户 open-websearch fixture

**Files:**
- Modify: `rust/crates/brain-integration-tests/tests/dynamic_tools.rs`
- Modify: `rust/crates/brain-integration-tests/Cargo.toml`

- [ ] **Step 1: 写 hermetic 热刷新集成测试**

测试不依赖用户机器配置，复制 stdio fixture 到临时目录：

```rust
#[tokio::test]
async fn mcp_plugin_and_skill_changes_appear_only_in_new_request_snapshots() {
    let fixture = DynamicRuntimeFixture::start_with_external_fixtures().await;
    let old = fixture.registry().snapshot();
    fixture.add_mcp_tool("new_search").await;
    fixture.enable_plugin("demo@external").await;
    fixture.add_skill("new-skill").await;
    fixture.runtime().reload(ReloadScope::All).await.assert_success();
    let new = fixture.registry().snapshot();
    assert!(old.resolve("mcp__fixture__new_search").is_none());
    assert!(old.resolve("plugin__demo__echo").is_none());
    assert!(new.resolve("mcp__fixture__new_search").is_some());
    assert!(new.resolve("plugin__demo__echo").is_some());
    assert!(new.resolve("Skill").is_some());
    assert!(new.version() > old.version());
}

#[tokio::test]
async fn disabled_or_disconnected_provider_is_not_advertised_to_next_request() {
    let fixture = DynamicRuntimeFixture::start_with_external_fixtures().await;
    fixture.disable_plugin("demo@external").await;
    fixture.disconnect_mcp("fixture").await;
    let view = fixture.new_request_view();
    assert!(view.snapshot().resolve("plugin__demo__echo").is_none());
    assert!(view.snapshot().resolve("mcp__fixture__echo").is_none());
}
```

- [ ] **Step 2: 运行 RED/GREEN**

Run: `cargo test -p brain-integration-tests --test dynamic_tools -- --nocapture`

Expected before final wiring: 外部动态断言失败。完成最小 wiring 后全部通过。

- [ ] **Step 3: 运行阶段门禁和真实配置只读预检**

Run:

```powershell
cargo tree -p brain-mcp -e features
cargo test -p brain-mcp -p runtime -p plugins -p brain-plugin
cargo test -p ai-brain-cli mcp --lib -- --nocapture
cargo test -p ai-brain-cli plugin --lib -- --nocapture
cargo test -p brain-integration-tests --test dynamic_tools -- --nocapture
cargo fmt --check
$config = Join-Path $env:USERPROFILE '.ai-brain\mcp\mcp-servers.json'
Test-Path -LiteralPath $config
Get-Content -LiteralPath $config | Select-String 'open-websearch'
```

Expected: 自动化门禁通过；当前机器配置存在且含 open-websearch。这里仅预检，不在测试中调用真实互联网，也不输出 env secret。

- [ ] **Step 4: 提交**

```powershell
git add crates/brain-integration-tests/Cargo.toml crates/brain-integration-tests/tests/dynamic_tools.rs
git diff --cached --check
git commit -m "test(v2): cover external provider hot reload"
```

阶段完成条件：stdio 与 Streamable HTTP 都是真调用；旧/新协议变化都能刷新；单 server 失败不拖垮其他能力；插件/技能热刷新进入下一请求；正常退出有界关闭所有进程与任务。
