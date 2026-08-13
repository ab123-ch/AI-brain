# 智脑 v2 核心工具注册表与 MainBrain 请求快照 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 建立定义、权限和执行 route 同源的版本化工具快照，并让 MainBrain 的同步、流式、压缩重入和测试适配路径在一次请求内只使用一个快照。

**Architecture:** `brain-core` 提供不依赖后端和 LLM crate 的注册合同，`brain-motor` 用一个 `RwLock<RegistryState>` 原子替换 `Arc<ToolSnapshot>`。`brain-main` 把 snapshot descriptor 投影为 LLM `ToolDefinition`，tool loop 直接解析并执行 snapshot 内 route；兼容构造器通过 `StaticToolRegistry` 建立测试快照，不再维护独立定义数组。

**Tech Stack:** Rust 2021、Tokio futures、serde、thiserror、sha2、url、std `RwLock<Arc<_>>`、Cargo tests。

---

## 文件结构

- Create: `rust/crates/brain-core/src/tool_registry.rs` — 核心类型、route/provider/admin/view 合同、不可变 snapshot/request view、静态测试 registry。
- Modify: `rust/crates/brain-core/Cargo.toml` — 增加名称哈希与 URL 参数 guard 的直接依赖。
- Modify: `rust/crates/brain-core/src/lib.rs` — 导出 `tool_registry`。
- Replace: `rust/crates/brain-motor/src/tool_registry.rs` — 实现动态 registry，不再写死工具名。
- Modify: `rust/crates/brain-motor/src/lib.rs` — 导出 `DynamicToolRegistry`。
- Create: `rust/crates/brain-core/src/tool_policy.rs` — metadata + canonical guard profile + 用户确认的共享策略顺序。
- Modify: `rust/crates/brain-core/src/guard_check.rs` — 增加不依赖公开别名的 profile guard，保留旧名称入口兼容。
- Modify: `rust/crates/brain-core/src/types.rs` — 增加显式工具确认事件。
- Modify: `rust/crates/brain-main/src/lib.rs` — 导出新构造入口需要的类型。
- Modify: `rust/crates/brain-main/src/main_brain.rs` — 持有 registry view，入口固定 request view。
- Modify: `rust/crates/brain-main/src/tool_loop.rs` — 同快照广告、校验、route、hooks、系统 route。
- Modify: `rust/crates/ai-brain-cli/src/terminal.rs` — 终端确认处理。
- Modify: `rust/crates/ai-brain-cli/src/tui/app.rs` — TUI 确认处理。
- Modify: `rust/crates/ai-brain-cli/src/tui/output.rs` — 确认事件展示。
- Modify: `rust/crates/ai-brain-cli/src/tui/session_logger.rs` — 确认事件脱敏日志。
- Modify: `rust/crates/ai-brain-cli/src/main.rs` — 非交互命令显式拒绝确认请求，避免挂起。
- Modify: `rust/crates/ai-brain-cli/src/web/progress_adapter.rs` — Web 可序列化确认事件。
- Modify: `rust/crates/ai-brain-cli/src/web/ws_handler.rs` — pending approval 映射与客户端响应。
- Modify: `rust/crates/ai-brain-cli/src/web/static/app.js` — Web 确认面板和响应消息。
- Modify: `rust/crates/brain-main/Cargo.toml` — 仅为测试增加 `brain-motor` dev-dependency。

### Task 1: 定义核心注册合同与结构化 route 错误

**Files:**
- Create: `rust/crates/brain-core/src/tool_registry.rs`
- Modify: `rust/crates/brain-core/Cargo.toml`
- Modify: `rust/crates/brain-core/src/lib.rs`
- Modify: `rust/Cargo.lock`

- [ ] **Step 1: 写核心合同的编译型失败测试**

在新文件测试模块先写以下测试，要求 registration 必须携带 route，并验证错误分类和名称规范化：

```rust
#[test]
fn registration_keeps_descriptor_metadata_and_route_together() {
    let route: Arc<dyn ToolRoute> = Arc::new(RecordingRoute::new("ok"));
    let registration = ToolRegistration::new(
        "builtin",
        ToolSourceKind::Builtin,
        "read_file",
        ToolDescriptor {
            name: "read_file".into(),
            description: "读取文件".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        ToolMetadata::read_only(ToolExposure::Base),
        route,
    )
    .unwrap();

    assert_eq!(registration.canonical_name, "read_file");
    assert_eq!(registration.route.kind(), ToolRouteKind::Backend);
    assert_eq!(registration.metadata.permission, ToolPermission::ReadOnly);
}

#[test]
fn normalize_tool_segment_is_deterministic_and_rejects_empty_values() {
    assert_eq!(normalize_tool_segment("Open Web/Search"), "open_web_search");
    assert_eq!(normalize_tool_segment("a__b"), "a_b");
    assert_eq!(normalize_tool_segment("___"), "");
    assert!(ToolRegistration::validate_name("").is_err());
    assert!(ToolRegistration::validate_name("bad.name").is_err());
    assert!(ToolRegistration::validate_name(&"x".repeat(65)).is_err());
}

#[test]
fn qualified_tool_name_is_model_safe_bounded_and_stable() {
    let long_slash = "find/".repeat(30);
    let long_space = "find ".repeat(30);
    let first = qualify_tool_name("mcp", &["Docs Server", &long_slash]).unwrap();
    let second = qualify_tool_name("mcp", &["Docs Server", &long_slash]).unwrap();
    let collision = qualify_tool_name("mcp", &["Docs Server", &long_space]).unwrap();
    assert_eq!(first, second);
    assert_ne!(first, collision);
    assert!(first.len() <= 64);
    assert!(first.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')));
}
```

测试内定义 `RecordingRoute`，其 `execute` 返回 `ToolExecutionResult { tool_name, output: "ok", is_error: false, duration_ms: 0 }`。

- [ ] **Step 2: 运行测试并确认 RED**

Run: `cargo test -p brain-core tool_registry --lib -- --nocapture`

Working directory: `rust/`

Expected: 编译失败，`brain_core::tool_registry` 及相关类型尚不存在。

- [ ] **Step 3: 增加依赖并写核心枚举、metadata、route 和 registration**

在 `brain-core/Cargo.toml` 增加 `sha2 = "0.10"`。在 `tool_registry.rs` 写入以下公开合同；字段名在后续任务中保持不变：

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::tool_executor::{ToolExecutionContext, ToolExecutor};
use crate::types::{ToolCall, ToolDescriptor, ToolExecutionResult};

pub type ToolFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ToolPermission {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolRiskLevel { Low, Medium, High }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolExposure { Base, Deferred, Internal }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolSourceKind { Builtin, Mcp, Plugin, Session }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolGuardProfile { None, ShellCommand, SensitivePath, RemoteUrl }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRouteKind { Backend, AskUser, ToolSearch }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolRouteErrorKind {
    Unavailable,
    InvalidInput,
    PermissionDenied,
    Timeout,
    Transport,
    Protocol,
    Backend,
}

#[derive(Debug, Clone, Error)]
#[error("{kind:?}: {message}")]
pub struct ToolRouteError {
    pub kind: ToolRouteErrorKind,
    pub message: String,
}

impl ToolRouteError {
    pub fn new(kind: ToolRouteErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

pub trait ToolRoute: Send + Sync {
    fn kind(&self) -> ToolRouteKind { ToolRouteKind::Backend }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        context: &'a ToolExecutionContext,
    ) -> ToolFuture<'a, Result<ToolExecutionResult, ToolRouteError>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolMetadata {
    pub permission: ToolPermission,
    pub risk_level: ToolRiskLevel,
    pub exposure: ToolExposure,
    pub side_effecting: bool,
    pub network_access: bool,
    pub requires_confirmation: bool,
    pub guard_profile: ToolGuardProfile,
    pub search_terms: Vec<String>,
    pub scenarios: Vec<String>,
}

impl ToolMetadata {
    pub fn read_only(exposure: ToolExposure) -> Self {
        Self {
            permission: ToolPermission::ReadOnly,
            risk_level: ToolRiskLevel::Low,
            exposure,
            side_effecting: false,
            network_access: false,
            requires_confirmation: false,
            guard_profile: ToolGuardProfile::None,
            search_terms: Vec::new(),
            scenarios: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolPromptKind { Bootstrap, SkillsSummary }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPromptFragment {
    pub kind: ToolPromptKind,
    pub content: String,
}

pub struct ToolRegistration {
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub descriptor: ToolDescriptor,
    pub provider_id: String,
    pub source_kind: ToolSourceKind,
    pub source_instance: String,
    pub metadata: ToolMetadata,
    pub route: Arc<dyn ToolRoute>,
}

impl ToolRegistration {
    pub fn new(
        provider_id: impl Into<String>,
        source_kind: ToolSourceKind,
        canonical_name: impl Into<String>,
        mut descriptor: ToolDescriptor,
        metadata: ToolMetadata,
        route: Arc<dyn ToolRoute>,
    ) -> Result<Self, ToolRegistryError> {
        let canonical_name = canonical_name.into();
        Self::validate_name(&canonical_name)?;
        descriptor.name.clone_from(&canonical_name);
        Ok(Self {
            canonical_name,
            aliases: Vec::new(),
            descriptor,
            provider_id: provider_id.into(),
            source_kind,
            source_instance: String::new(),
            metadata,
            route,
        })
    }

    pub fn validate_name(name: &str) -> Result<(), ToolRegistryError> {
        let valid = !name.is_empty()
            && name.len() <= 64
            && name.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'));
        if !valid {
            Err(ToolRegistryError::InvalidName(name.to_string()))
        } else {
            Ok(())
        }
    }

    pub fn with_aliases(mut self, aliases: Vec<String>) -> Result<Self, ToolRegistryError> {
        let mut seen = BTreeSet::new();
        for alias in &aliases {
            Self::validate_name(alias)?;
            if alias == &self.canonical_name || !seen.insert(alias.clone()) {
                return Err(ToolRegistryError::AliasConflict(alias.clone()));
            }
        }
        self.aliases = aliases;
        Ok(self)
    }
}

pub fn normalize_tool_segment(value: &str) -> String {
    let mut normalized = String::new();
    let mut separator = false;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            separator = false;
        } else if !separator && !normalized.is_empty() {
            normalized.push('_');
            separator = true;
        }
    }
    normalized.trim_matches('_').to_string()
}

pub fn qualify_tool_name(prefix: &str, raw_segments: &[&str]) -> Result<String, ToolRegistryError> {
    let mut segments = Vec::with_capacity(raw_segments.len() + 1);
    segments.push(normalize_tool_segment(prefix));
    segments.extend(raw_segments.iter().map(|value| normalize_tool_segment(value)));
    if segments.iter().any(String::is_empty) {
        return Err(ToolRegistryError::InvalidName(raw_segments.join("/")));
    }
    let candidate = segments.join("__");
    if candidate.len() <= 64 {
        return Ok(candidate);
    }
    let raw_identity = std::iter::once(prefix).chain(raw_segments.iter().copied())
        .collect::<Vec<_>>().join("\0");
    disambiguate_tool_name(&candidate, &raw_identity)
}

pub fn disambiguate_tool_name(base: &str, raw_identity: &str) -> Result<String, ToolRegistryError> {
    let digest = sha2::Sha256::digest(raw_identity.as_bytes());
    let suffix = digest[..4].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let keep = base.len().min(55);
    let mut shortened = base[..keep].trim_end_matches('_').to_string();
    shortened.push('_');
    shortened.push_str(&suffix);
    ToolRegistration::validate_name(&shortened)?;
    Ok(shortened)
}
```

为调用 `Sha256::digest` 导入 `sha2::Digest`。截断位置只能落在上述 normalization 产生的 ASCII 上；短名称发生 normalization 碰撞时，Provider 的名称映射同样调用 `disambiguate_tool_name(base, raw_identity)`，不能自行拼接无界后缀。完整 raw 名称仍保存在 `source_instance`/Provider 路由映射中，执行时不得从截断名反解。64 字符上限取当前 LLM function-tool 兼容交集，避免动态 MCP/插件名在发送请求时才被 Provider API 拒绝。

同时定义完整错误枚举，后续任务不再临时扩展名字：

```rust
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolRegistryError {
    #[error("工具名称无效: {0}")]
    InvalidName(String),
    #[error("工具 canonical 名称冲突: {0}")]
    CanonicalConflict(String),
    #[error("工具别名冲突: {0}")]
    AliasConflict(String),
    #[error("工具与 Provider 不匹配: {0}")]
    ProviderMismatch(String),
    #[error("工具 {0} 不是 deferred 能力")]
    InvalidExposure(String),
    #[error("工具注册表锁已损坏")]
    Poisoned,
    #[error("工具注册表版本溢出")]
    VersionOverflow,
    #[error("工具 Provider 指纹生成失败: {0}")]
    Fingerprint(String),
}
```

为 `ToolRegistration` 写只显示 name/provider/source、绝不格式化 route 的手工 `Debug`。

- [ ] **Step 4: 增加 executor 与系统 route adapter**

同文件加入：

```rust
pub struct ExecutorToolRoute {
    canonical_name: String,
    executor: Arc<dyn ToolExecutor>,
}

impl ExecutorToolRoute {
    pub fn new(canonical_name: impl Into<String>, executor: Arc<dyn ToolExecutor>) -> Self {
        Self { canonical_name: canonical_name.into(), executor }
    }
}

impl ToolRoute for ExecutorToolRoute {
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        context: &'a ToolExecutionContext,
    ) -> ToolFuture<'a, Result<ToolExecutionResult, ToolRouteError>> {
        let mut canonical_call = call.clone();
        canonical_call.tool_name.clone_from(&self.canonical_name);
        Box::pin(async move {
            Ok(self.executor.execute_with_context(&canonical_call, context).await)
        })
    }
}

pub struct SystemToolRoute(pub ToolRouteKind);

impl ToolRoute for SystemToolRoute {
    fn kind(&self) -> ToolRouteKind { self.0 }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _context: &'a ToolExecutionContext,
    ) -> ToolFuture<'a, Result<ToolExecutionResult, ToolRouteError>> {
        Box::pin(async move {
            Err(ToolRouteError::new(
                ToolRouteErrorKind::Protocol,
                format!("系统工具 {} 必须由 tool loop 执行", call.tool_name),
            ))
        })
    }
}
```

- [ ] **Step 5: 导出模块并运行 GREEN**

在 `brain-core/src/lib.rs` 增加 `pub mod tool_registry;`。

Run: `cargo test -p brain-core tool_registry --lib -- --nocapture`

Expected: 新测试通过，`cargo clippy -p brain-core --all-targets -- -D warnings` 退出 0。

- [ ] **Step 6: 提交**

```powershell
git add crates/brain-core/Cargo.toml crates/brain-core/src/lib.rs crates/brain-core/src/tool_registry.rs Cargo.lock
git commit -m "feat(core): define dynamic tool contracts"
```

### Task 2: 构建不可变 ToolSnapshot、RequestView 与 Provider 合同

**Files:**
- Modify: `rust/crates/brain-core/src/tool_registry.rs`

- [ ] **Step 1: 写 snapshot/alias/exposure/session 的失败测试**

加入四个测试：

```rust
#[test]
fn snapshot_resolves_canonical_and_unique_alias_to_same_registration() {
    let snapshot = snapshot_from(vec![registration("plugin__demo__read", ToolExposure::Base)
        .with_aliases(vec!["demo_read".into()]).unwrap()]);
    assert!(Arc::ptr_eq(
        &snapshot.resolve("plugin__demo__read").unwrap(),
        &snapshot.resolve("demo_read").unwrap(),
    ));
}

#[test]
fn conflicting_alias_is_rejected_but_both_canonical_tools_survive() {
    let snapshot = snapshot_from(vec![
        registration("plugin__a__echo", ToolExposure::Base)
            .with_aliases(vec!["echo".into()]).unwrap(),
        registration("plugin__b__echo", ToolExposure::Base)
            .with_aliases(vec!["echo".into()]).unwrap(),
    ]);
    assert!(snapshot.resolve("plugin__a__echo").is_some());
    assert!(snapshot.resolve("plugin__b__echo").is_some());
    assert!(snapshot.resolve("echo").is_none());
    assert!(snapshot.diagnostics().iter().any(|item| item.code == ToolDiagnosticCode::AliasConflict));
}

#[test]
fn alias_that_matches_another_canonical_is_rejected_but_canonicals_survive() {
    let snapshot = snapshot_from(vec![
        registration("read_file", ToolExposure::Base),
        registration("plugin__demo__read", ToolExposure::Base)
            .with_aliases(vec!["read_file".into()]).unwrap(),
    ]);
    assert_eq!(snapshot.resolve("read_file").unwrap().canonical_name, "read_file");
    assert!(snapshot.resolve("plugin__demo__read").is_some());
    assert!(snapshot.diagnostics().iter().any(|item| {
        item.code == ToolDiagnosticCode::AliasConflict
            && item.tool_name.as_deref() == Some("read_file")
    }));
}

#[test]
fn request_view_advertises_base_but_not_deferred_or_internal() {
    let snapshot = snapshot_from(vec![
        registration("base", ToolExposure::Base),
        registration("later", ToolExposure::Deferred),
        registration("hidden", ToolExposure::Internal),
    ]);
    let view = ToolRequestView::base(snapshot);
    assert_eq!(view.descriptors().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["base"]);
    assert!(view.resolve_advertised("later").is_none());
}

#[test]
fn activating_deferred_changes_only_the_request_view() {
    let snapshot = snapshot_from(vec![registration("later", ToolExposure::Deferred)]);
    let mut first = ToolRequestView::base(Arc::clone(&snapshot));
    let second = ToolRequestView::base(snapshot);
    first.activate_deferred(&["later".into()]).unwrap();
    assert!(first.resolve_advertised("later").is_some());
    assert!(second.resolve_advertised("later").is_none());
}

#[test]
fn session_derivation_rejects_collision_without_mutating_base() {
    let base = snapshot_from(vec![registration("read_file", ToolExposure::Base)]);
    let session = ProviderContribution::ready(
        "session:room-1",
        vec![Arc::new(registration("read_file", ToolExposure::Base))],
    );
    assert!(base.derive_session(session).is_err());
    assert!(base.resolve("read_file").is_some());
}

#[test]
fn contribution_fingerprint_is_canonical_and_tracks_route_generation() {
    let left_schema = serde_json::json!({"type":"object","properties":{"b":{"type":"number"},"a":{"type":"string"}}});
    let right_schema = serde_json::json!({"properties":{"a":{"type":"string"},"b":{"type":"number"}},"type":"object"});
    let mut left = contribution_with_schema("builtin", "route-v1", left_schema);
    let mut right = contribution_with_schema("builtin", "route-v1", right_schema);
    left.health.updated_at = chrono::Utc::now() - chrono::Duration::hours(1);
    right.health.updated_at = chrono::Utc::now();
    assert_eq!(contribution_fingerprint(&left).unwrap(), contribution_fingerprint(&right).unwrap());
    right.generation = "route-v2".into();
    assert_ne!(contribution_fingerprint(&left).unwrap(), contribution_fingerprint(&right).unwrap());
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-core snapshot_ --lib -- --nocapture`

Expected: 编译失败，snapshot/provider/request view 尚未定义。

- [ ] **Step 3: 实现 ProviderContribution 与 provider/view/admin trait**

写入以下类型：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderState { Ready, Degraded, Error, Stopped }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHealth {
    pub state: ProviderState,
    pub message: Option<String>,
    pub last_success_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolDiagnosticCode {
    AliasConflict,
    Configuration,
    Discovery,
    Refresh,
    BackendSwitch,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDiagnostic {
    pub code: ToolDiagnosticCode,
    pub provider_id: Option<String>,
    pub tool_name: Option<String>,
    pub message: String,
}

#[derive(Clone)]
pub struct ProviderContribution {
    pub provider_id: String,
    pub generation: String,
    pub registrations: Vec<Arc<ToolRegistration>>,
    pub prompt_fragments: Vec<ToolPromptFragment>,
    pub health: ProviderHealth,
    pub diagnostics: Vec<ToolDiagnostic>,
}

impl ProviderContribution {
    pub fn ready(provider_id: impl Into<String>, registrations: Vec<Arc<ToolRegistration>>) -> Self {
        Self {
            provider_id: provider_id.into(),
            generation: "static".into(),
            registrations,
            prompt_fragments: Vec::new(),
            health: ProviderHealth {
                state: ProviderState::Ready,
                message: None,
                last_success_at: Some(chrono::Utc::now()),
                updated_at: chrono::Utc::now(),
            },
            diagnostics: Vec::new(),
        }
    }

    pub fn with_generation(mut self, generation: impl Into<String>) -> Self {
        self.generation = generation.into();
        self
    }
}

pub trait ToolProvider: Send + Sync {
    fn id(&self) -> &str;
    fn discover(&self) -> ToolFuture<'_, Result<ProviderContribution, ToolProviderError>>;
    fn shutdown(&self) -> ToolFuture<'_, Result<(), ToolProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

pub trait ToolRegistryView: Send + Sync {
    fn snapshot(&self) -> Arc<ToolSnapshot>;
}

pub trait ToolRegistryAdmin: ToolRegistryView {
    fn replace_provider(
        &self,
        contribution: ProviderContribution,
    ) -> Result<Arc<ToolSnapshot>, ToolRegistryError>;
    fn remove_provider(&self, provider_id: &str) -> Result<Arc<ToolSnapshot>, ToolRegistryError>;
    fn mark_provider_error(
        &self,
        provider_id: &str,
        message: String,
        retain_last_good: bool,
    ) -> Result<Arc<ToolSnapshot>, ToolRegistryError>;
}
```

Provider 后端错误与 registry 结构错误分开，避免把网络/插件失败伪装成名称冲突：

```rust
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("工具 Provider {provider_id} {stage:?} 失败: {message}")]
pub struct ToolProviderError {
    pub provider_id: String,
    pub stage: ToolProviderStage,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProviderStage { Configure, Discover, Initialize, Execute, Shutdown }
```

`ToolProviderError::new` 必须接收已经脱敏的 message；各 adapter 在自身错误边界完成 secret redaction。`ToolRegistryError` 只表示注册数据/锁/version 问题，不包装任意后端输出。

- [ ] **Step 4: 实现 snapshot 的确定性构建与派生**

`ToolSnapshot` 使用以下字段和算法：

```rust
pub struct ToolSnapshot {
    version: u64,
    fingerprint: [u8; 32],
    registrations: Vec<Arc<ToolRegistration>>,
    canonical_index: BTreeMap<String, usize>,
    public_index: BTreeMap<String, usize>,
    provider_health: BTreeMap<String, ProviderHealth>,
    prompt_fragments: Vec<ToolPromptFragment>,
    diagnostics: Vec<ToolDiagnostic>,
    contributions: BTreeMap<String, ProviderContribution>,
}

impl ToolSnapshot {
    pub fn empty() -> Self { Self::build(0, BTreeMap::new()).expect("空快照必须有效") }

    pub fn build(
        version: u64,
        contributions: BTreeMap<String, ProviderContribution>,
    ) -> Result<Self, ToolRegistryError> {
        let mut registrations = contributions.values()
            .flat_map(|item| item.registrations.iter().cloned())
            .collect::<Vec<_>>();
        registrations.sort_by(|left, right| left.canonical_name.cmp(&right.canonical_name));

        let mut canonical_index = BTreeMap::new();
        let mut public_index = BTreeMap::new();
        let mut alias_owners = BTreeMap::<String, Vec<usize>>::new();
        for (index, registration) in registrations.iter().enumerate() {
            if registration.provider_id.is_empty() {
                return Err(ToolRegistryError::ProviderMismatch(registration.canonical_name.clone()));
            }
            if canonical_index.insert(registration.canonical_name.clone(), index).is_some() {
                return Err(ToolRegistryError::CanonicalConflict(registration.canonical_name.clone()));
            }
            if public_index.insert(registration.canonical_name.clone(), index).is_some() {
                return Err(ToolRegistryError::CanonicalConflict(registration.canonical_name.clone()));
            }
            for alias in &registration.aliases {
                alias_owners.entry(alias.clone()).or_default().push(index);
            }
        }

        let mut diagnostics = contributions.values()
            .flat_map(|item| item.diagnostics.iter().cloned())
            .collect::<Vec<_>>();
        for (alias, owners) in alias_owners {
            if owners.len() == 1 && !public_index.contains_key(&alias) {
                public_index.insert(alias, owners[0]);
            } else {
                diagnostics.push(ToolDiagnostic {
                    code: ToolDiagnosticCode::AliasConflict,
                    provider_id: None,
                    tool_name: Some(alias.clone()),
                    message: format!("别名 {alias} 存在冲突，已拒绝该别名并保留 canonical 工具"),
                });
            }
        }

        let provider_health = contributions.iter()
            .map(|(id, item)| (id.clone(), item.health.clone()))
            .collect();
        let prompt_fragments = contributions.values()
            .flat_map(|item| item.prompt_fragments.iter().cloned())
            .collect();
        Ok(Self {
            version,
            fingerprint: stable_snapshot_fingerprint(&contributions)?,
            registrations,
            canonical_index,
            public_index,
            provider_health,
            prompt_fragments,
            diagnostics,
            contributions,
        })
    }

    pub fn version(&self) -> u64 { self.version }
    pub fn fingerprint(&self) -> [u8; 32] { self.fingerprint }
    pub fn registrations(&self) -> &[Arc<ToolRegistration>] { &self.registrations }
    pub fn prompt_fragments(&self) -> &[ToolPromptFragment] { &self.prompt_fragments }
    pub fn provider_health(&self) -> &BTreeMap<String, ProviderHealth> { &self.provider_health }
    pub fn diagnostics(&self) -> &[ToolDiagnostic] { &self.diagnostics }

    pub fn resolve(&self, public_name: &str) -> Option<Arc<ToolRegistration>> {
        self.public_index.get(public_name).map(|index| Arc::clone(&self.registrations[*index]))
    }

    pub fn derive_session(
        &self,
        contribution: ProviderContribution,
    ) -> Result<Arc<Self>, ToolRegistryError> {
        if contribution.generation.is_empty()
            || contribution.provider_id.is_empty()
            || !contribution.provider_id.starts_with("session:")
        {
            return Err(ToolRegistryError::ProviderMismatch(contribution.provider_id));
        }
        let session_fingerprint = contribution_fingerprint(&contribution)?;
        let mut contributions = self.contributions.clone();
        contributions.insert(contribution.provider_id.clone(), contribution);
        let mut derived = Self::build(self.version, contributions)?;
        derived.fingerprint = hash_global_and_session(self.fingerprint, session_fingerprint);
        Ok(Arc::new(derived))
    }
}
```

在 `brain-core` 同文件定义唯一的稳定指纹实现，motor 只调用它，禁止复制一套算法或形成 core → motor 反向依赖：

```rust
pub fn contribution_fingerprint(
    contribution: &ProviderContribution,
) -> Result<[u8; 32], ToolRegistryError> {
    let mut registrations = contribution.registrations.iter().map(|registration| {
        let mut aliases = registration.aliases.clone();
        aliases.sort();
        let mut search_terms = registration.metadata.search_terms.clone();
        search_terms.sort();
        let mut scenarios = registration.metadata.scenarios.clone();
        scenarios.sort();
        serde_json::json!({
            "canonical_name": registration.canonical_name.as_str(),
            "aliases": aliases,
            "descriptor": {
                "name": registration.descriptor.name.as_str(),
                "description": registration.descriptor.description.as_str(),
                "input_schema": &registration.descriptor.input_schema,
            },
            "provider_id": registration.provider_id.as_str(),
            "source_kind": registration.source_kind,
            "source_instance": registration.source_instance.as_str(),
            "metadata": {
                "permission": registration.metadata.permission,
                "risk_level": registration.metadata.risk_level,
                "exposure": registration.metadata.exposure,
                "side_effecting": registration.metadata.side_effecting,
                "network_access": registration.metadata.network_access,
                "requires_confirmation": registration.metadata.requires_confirmation,
                "guard_profile": registration.metadata.guard_profile,
                "search_terms": search_terms,
                "scenarios": scenarios,
            },
        })
    }).collect::<Vec<_>>();
    registrations.sort_by(|left, right| {
        left["canonical_name"].as_str().cmp(&right["canonical_name"].as_str())
    });

    let mut prompts = contribution.prompt_fragments.clone();
    prompts.sort_by(|left, right| (left.kind, left.content.as_str()).cmp(&(right.kind, right.content.as_str())));
    let mut diagnostics = contribution.diagnostics.clone();
    diagnostics.sort_by(|left, right| {
        (left.code, left.provider_id.as_deref(), left.tool_name.as_deref(), left.message.as_str())
            .cmp(&(right.code, right.provider_id.as_deref(), right.tool_name.as_deref(), right.message.as_str()))
    });
    let view = serde_json::json!({
        "provider_id": contribution.provider_id.as_str(),
        "generation": contribution.generation.as_str(),
        "registrations": registrations,
        "prompt_fragments": prompts,
        "health": {
            "state": contribution.health.state,
            "message": contribution.health.message.as_deref(),
        },
        "diagnostics": diagnostics,
    });
    hash_json(&canonicalize_json(&view))
}

fn stable_snapshot_fingerprint(
    contributions: &BTreeMap<String, ProviderContribution>,
) -> Result<[u8; 32], ToolRegistryError> {
    let mut hasher = sha2::Sha256::new();
    for (provider_id, contribution) in contributions {
        let digest = contribution_fingerprint(contribution)?;
        hasher.update((provider_id.len() as u64).to_be_bytes());
        hasher.update(provider_id.as_bytes());
        hasher.update(digest);
    }
    Ok(hasher.finalize().into())
}

fn canonicalize_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut normalized = serde_json::Map::new();
            for key in keys {
                normalized.insert(key.clone(), canonicalize_json(&object[key]));
            }
            serde_json::Value::Object(normalized)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize_json).collect())
        }
        scalar => scalar.clone(),
    }
}

fn hash_json(value: &serde_json::Value) -> Result<[u8; 32], ToolRegistryError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| ToolRegistryError::Fingerprint(error.to_string()))?;
    Ok(sha2::Sha256::digest(bytes).into())
}

fn hash_global_and_session(global: [u8; 32], derived: [u8; 32]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    for field in [global.as_slice(), derived.as_slice()] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    hasher.finalize().into()
}
```

为以上实现导入 `sha2::Digest`。`ToolPromptKind`、`ToolDiagnosticCode` 需派生 `PartialOrd, Ord`；指纹明确排除 route 指针、`last_success_at`、`updated_at` 和 snapshot version。`canonicalize_json` 递归排序 JSON object key、保留 array 顺序；registration/alias/search/scenario/prompt/diagnostic 则按上面的语义键排序。

在遍历 contribution 时另校验 `registration.provider_id == contribution.provider_id`，并拒绝空 `generation`。这一步要在任何 index 写入前完成，错误时不返回半成品。全局 build 的 fingerprint 包含所有 provider generation/registration/prompt/health state+message/diagnostics；session 派生再把 base fingerprint 与派生 fingerprint 做长度前缀哈希。只有 canonical 与 canonical 冲突才使候选提交失败；alias 与 canonical 冲突、alias 与 alias 冲突都只拒绝冲突 alias，保留所有 canonical 并记录诊断。单条 registration 内 alias 重复或等于自身 canonical 则由 `with_aliases` 直接返回 `AliasConflict`。

- [ ] **Step 5: 实现 RequestView 的广告与 deferred 激活**

```rust
#[derive(Clone)]
pub struct ToolRequestView {
    snapshot: Arc<ToolSnapshot>,
    advertised: BTreeSet<String>,
}

impl ToolRequestView {
    pub fn base(snapshot: Arc<ToolSnapshot>) -> Self {
        let advertised = snapshot.registrations().iter()
            .filter(|item| item.metadata.exposure == ToolExposure::Base)
            .flat_map(|item| std::iter::once(item.canonical_name.clone()).chain(item.aliases.clone()))
            .collect();
        Self { snapshot, advertised }
    }

    pub fn empty(snapshot: Arc<ToolSnapshot>) -> Self {
        Self { snapshot, advertised: BTreeSet::new() }
    }

    pub fn snapshot(&self) -> &Arc<ToolSnapshot> { &self.snapshot }

    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.advertised.iter().filter_map(|public_name| {
            self.snapshot.resolve(public_name).map(|registration| {
                let mut descriptor = registration.descriptor.clone();
                descriptor.name.clone_from(public_name);
                descriptor
            })
        }).collect()
    }

    pub fn resolve_advertised(&self, public_name: &str) -> Option<Arc<ToolRegistration>> {
        self.advertised.contains(public_name).then(|| self.snapshot.resolve(public_name)).flatten()
    }

    pub fn activate_deferred(&mut self, names: &[String]) -> Result<(), ToolRegistryError> {
        for name in names {
            let registration = self.snapshot.resolve(name)
                .ok_or_else(|| ToolRegistryError::InvalidName(name.clone()))?;
            if registration.metadata.exposure != ToolExposure::Deferred {
                return Err(ToolRegistryError::InvalidExposure(name.clone()));
            }
            self.advertised.insert(registration.canonical_name.clone());
            self.advertised.extend(registration.aliases.iter().cloned());
        }
        Ok(())
    }

    pub fn deferred_matches(&self, query: &str, limit: usize) -> Vec<Arc<ToolRegistration>> {
        let tokens = query.to_ascii_lowercase();
        self.snapshot.registrations().iter()
            .filter(|item| item.metadata.exposure == ToolExposure::Deferred)
            .filter(|item| {
                item.canonical_name.to_ascii_lowercase().contains(&tokens)
                    || item.descriptor.description.to_ascii_lowercase().contains(&tokens)
                    || item.metadata.search_terms.iter().any(|term| term.to_ascii_lowercase().contains(&tokens))
                    || item.metadata.scenarios.iter().any(|term| term.to_ascii_lowercase().contains(&tokens))
            })
            .take(limit.max(1))
            .cloned()
            .collect()
    }
}
```

`InvalidExposure` 已在 Task 1 的固定错误枚举中定义，不新增另一套错误类型。

- [ ] **Step 6: 运行测试并提交**

Run:

```powershell
cargo test -p brain-core tool_registry --lib -- --nocapture
cargo clippy -p brain-core --all-targets -- -D warnings
```

Expected: 全部通过。

```powershell
git add crates/brain-core/src/tool_registry.rs
git commit -m "feat(core): add immutable tool snapshots"
```

### Task 3: 用单锁实现 DynamicToolRegistry 原子替换

**Files:**
- Replace: `rust/crates/brain-motor/src/tool_registry.rs`
- Modify: `rust/crates/brain-motor/src/lib.rs`

- [ ] **Step 1: 将硬编码 registry 测试替换为动态 RED 合同**

删除依赖 `with_builtin_tools()` 的旧测试，新增：

```rust
#[test]
fn provider_replace_is_atomic_and_versions_are_monotonic() {
    let registry = DynamicToolRegistry::new();
    let v1 = registry.replace_provider(contribution("builtin", vec!["read_file"])).unwrap();
    let pinned = registry.snapshot();
    let v2 = registry.replace_provider(contribution("builtin", vec!["read_file", "glob_search"])).unwrap();
    assert_eq!(v1.version(), 1);
    assert_eq!(v2.version(), 2);
    assert!(pinned.resolve("glob_search").is_none());
    assert!(registry.snapshot().resolve("glob_search").is_some());
}

#[test]
fn identical_provider_contribution_is_a_noop() {
    let registry = DynamicToolRegistry::new();
    let first = registry.replace_provider(contribution("builtin", vec!["read_file"])).unwrap();
    let second = registry.replace_provider(contribution("builtin", vec!["read_file"])).unwrap();
    assert_eq!(second.version(), first.version());
}

#[test]
fn changed_route_generation_publishes_even_when_definition_is_identical() {
    let registry = DynamicToolRegistry::new();
    let first = registry.replace_provider(
        contribution("mcp:docs", vec!["mcp__docs__search"]).with_generation("connection-1")
    ).unwrap();
    let second = registry.replace_provider(
        contribution("mcp:docs", vec!["mcp__docs__search"]).with_generation("connection-2")
    ).unwrap();
    assert_eq!(second.version(), first.version() + 1);
}

#[test]
fn conflicting_candidate_keeps_last_healthy_snapshot() {
    let registry = DynamicToolRegistry::new();
    registry.replace_provider(contribution("builtin", vec!["read_file"])).unwrap();
    registry.replace_provider(contribution("plugin:a", vec!["plugin__a__read"])).unwrap();
    let before = registry.snapshot();
    let error = registry.replace_provider(contribution("plugin:b", vec!["read_file"])).unwrap_err();
    assert!(matches!(error, ToolRegistryError::CanonicalConflict(_)));
    assert_eq!(registry.snapshot().version(), before.version());
    assert!(registry.snapshot().resolve("plugin__a__read").is_some());
}

#[test]
fn provider_error_can_retain_or_revoke_last_good_tools() {
    let registry = DynamicToolRegistry::new();
    registry.replace_provider(contribution("mcp:docs", vec!["mcp__docs__search"])).unwrap();
    registry.mark_provider_error("mcp:docs", "断线".into(), true).unwrap();
    assert!(registry.snapshot().resolve("mcp__docs__search").is_some());
    registry.mark_provider_error("mcp:docs", "配置已删除".into(), false).unwrap();
    assert!(registry.snapshot().resolve("mcp__docs__search").is_none());
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-motor tool_registry --lib -- --nocapture`

Expected: 编译失败，旧 `ToolRegistry` 不实现新合同。

- [ ] **Step 3: 复用 core 指纹并实现单锁状态与两阶段提交**

用以下结构完整替换旧文件生产代码；指纹函数必须从 `brain-core` 导入：

```rust
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use brain_core::tool_registry::{
    contribution_fingerprint, ProviderContribution, ProviderHealth, ProviderState,
    ToolRegistryAdmin, ToolRegistryError, ToolRegistryView, ToolSnapshot,
};

struct RegistryState {
    current: Arc<ToolSnapshot>,
    contributions: BTreeMap<String, ProviderContribution>,
    fingerprints: BTreeMap<String, [u8; 32]>,
}

pub struct DynamicToolRegistry {
    state: RwLock<RegistryState>,
}

impl DynamicToolRegistry {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(RegistryState {
                current: Arc::new(ToolSnapshot::empty()),
                contributions: BTreeMap::new(),
                fingerprints: BTreeMap::new(),
            }),
        }
    }

    fn update_state<F>(&self, update: F) -> Result<Arc<ToolSnapshot>, ToolRegistryError>
    where
        F: FnOnce(
            &mut BTreeMap<String, ProviderContribution>,
            &mut BTreeMap<String, [u8; 32]>,
        ) -> Result<bool, ToolRegistryError>,
    {
        let mut state = self.state.write().map_err(|_| ToolRegistryError::Poisoned)?;
        let mut contributions = state.contributions.clone();
        let mut fingerprints = state.fingerprints.clone();
        if !update(&mut contributions, &mut fingerprints)? {
            return Ok(Arc::clone(&state.current));
        }
        let next_version = state.current.version().checked_add(1)
            .ok_or(ToolRegistryError::VersionOverflow)?;
        let candidate = Arc::new(ToolSnapshot::build(next_version, contributions.clone())?);
        state.contributions = contributions;
        state.fingerprints = fingerprints;
        state.current = Arc::clone(&candidate);
        Ok(candidate)
    }
}

impl Default for DynamicToolRegistry {
    fn default() -> Self { Self::new() }
}

impl ToolRegistryView for DynamicToolRegistry {
    fn snapshot(&self) -> Arc<ToolSnapshot> {
        match self.state.read() {
            Ok(state) => Arc::clone(&state.current),
            Err(poisoned) => {
                tracing::error!("动态工具注册表读锁已损坏，返回最后可见快照");
                Arc::clone(&poisoned.into_inner().current)
            }
        }
    }
}

impl ToolRegistryAdmin for DynamicToolRegistry {
    fn replace_provider(
        &self,
        contribution: ProviderContribution,
    ) -> Result<Arc<ToolSnapshot>, ToolRegistryError> {
        let provider_id = contribution.provider_id.clone();
        if contribution.generation.is_empty()
            || contribution.registrations.iter().any(|item| item.provider_id != provider_id)
        {
            return Err(ToolRegistryError::ProviderMismatch(provider_id));
        }
        self.update_state(move |candidate, fingerprints| {
            let fingerprint = contribution_fingerprint(&contribution)?;
            if fingerprints.get(&provider_id) == Some(&fingerprint) {
                return Ok(false);
            }
            candidate.insert(provider_id.clone(), contribution);
            fingerprints.insert(provider_id, fingerprint);
            Ok(true)
        })
    }

    fn remove_provider(&self, provider_id: &str) -> Result<Arc<ToolSnapshot>, ToolRegistryError> {
        let provider_id = provider_id.to_string();
        self.update_state(move |candidate, fingerprints| {
            let changed = candidate.remove(&provider_id).is_some();
            fingerprints.remove(&provider_id);
            Ok(changed)
        })
    }

    fn mark_provider_error(
        &self,
        provider_id: &str,
        message: String,
        retain_last_good: bool,
    ) -> Result<Arc<ToolSnapshot>, ToolRegistryError> {
        let provider_id = provider_id.to_string();
        self.update_state(move |candidate, fingerprints| {
            if retain_last_good {
                if let Some(previous) = candidate.get_mut(&provider_id) {
                    if previous.health.state == ProviderState::Degraded
                        && previous.health.message.as_deref() == Some(message.as_str())
                    {
                        return Ok(false);
                    }
                    let now = chrono::Utc::now();
                    previous.health = ProviderHealth {
                        state: ProviderState::Degraded,
                        message: Some(message.clone()),
                        last_success_at: previous.health.last_success_at,
                        updated_at: now,
                    };
                    previous.diagnostics.push(ToolDiagnostic {
                        code: ToolDiagnosticCode::Refresh,
                        provider_id: Some(provider_id.clone()),
                        tool_name: None,
                        message: message.clone(),
                    });
                    fingerprints.insert(
                        provider_id.clone(),
                        contribution_fingerprint(previous)?,
                    );
                    return Ok(true);
                }
            }
            let previous_last_success = candidate.get(&provider_id)
                .and_then(|previous| previous.health.last_success_at);
            if candidate.get(&provider_id).is_some_and(|previous| {
                previous.registrations.is_empty()
                    && previous.health.state == ProviderState::Error
                    && previous.health.message.as_deref() == Some(message.as_str())
            }) {
                return Ok(false);
            }
            let now = chrono::Utc::now();
            let revoked = ProviderContribution {
                provider_id: provider_id.clone(),
                generation: "revoked".into(),
                registrations: Vec::new(),
                prompt_fragments: Vec::new(),
                health: ProviderHealth {
                    state: ProviderState::Error,
                    message: Some(message.clone()),
                    last_success_at: previous_last_success,
                    updated_at: now,
                },
                diagnostics: vec![ToolDiagnostic {
                    code: ToolDiagnosticCode::Revoked,
                    provider_id: Some(provider_id.clone()),
                    tool_name: None,
                    message: message.clone(),
                }],
            };
            let fingerprint = contribution_fingerprint(&revoked)?;
            candidate.insert(provider_id.clone(), revoked);
            fingerprints.insert(provider_id, fingerprint);
            Ok(true)
        })
    }
}
```

`update_state` 是唯一写入口：取得 write lock 后复制当前 contributions、应用纯内存修改、构建完整候选快照，成功后才同时替换 contributions/current。稳定序列化与 SHA-256 算法只使用 Task 2 的 `brain_core::tool_registry::contribution_fingerprint`，motor 不自行实现第二套算法。

相同状态、相同脱敏 message 的重复故障报告是 no-op，不再追加重复 diagnostic，也不递增 registry version。恢复 Ready 会改变 state/message fingerprint；timestamps 只随真正语义提交更新，不能让 wall clock 本身造成假刷新。

`generation` 是 route 后端身份的一部分：Builtin 可用固定配置指纹；MCP 使用连接 generation + 完整目录 revision；Plugin 使用 lifecycle generation；Web 门面使用固定 backend-chain generation。这样 descriptor 不变但连接/进程已经替换时仍发布新 route。`replace_provider` fingerprint 与当前相同则直接返回当前 Arc，不递增 version。Provider 的 I/O 必须在调用该方法前完成，因此临界区短且并发 writers 不会丢更新；构建失败时 state 保持原样。

- [ ] **Step 4: 增加并发读写测试**

```rust
#[test]
fn concurrent_readers_never_observe_partial_provider_sets() {
    let registry = Arc::new(DynamicToolRegistry::new());
    registry.replace_provider(contribution("builtin", vec!["a", "b"])).unwrap();
    let reader = Arc::clone(&registry);
    let handle = std::thread::spawn(move || {
        for _ in 0..2_000 {
            let snapshot = reader.snapshot();
            let count = ["a", "b", "c"].into_iter()
                .filter(|name| snapshot.resolve(name).is_some()).count();
            assert!(count == 2 || count == 3);
        }
    });
    registry.replace_provider(contribution("builtin", vec!["a", "b", "c"])).unwrap();
    handle.join().unwrap();
}
```

- [ ] **Step 5: 导出并运行 GREEN**

Run:

```powershell
cargo test -p brain-motor tool_registry --lib -- --nocapture
cargo clippy -p brain-motor --all-targets -- -D warnings
```

Expected: 版本、冲突、错误保留/撤销和并发测试全部通过。

- [ ] **Step 6: 提交**

```powershell
git add crates/brain-motor/src/tool_registry.rs crates/brain-motor/src/lib.rs
git commit -m "feat(motor): add atomic dynamic tool registry"
```

### Task 4: 在 brain-core 建立共享工具权限、参数 guard 与确认策略

**Files:**
- Create: `rust/crates/brain-core/src/tool_policy.rs`
- Modify: `rust/crates/brain-core/Cargo.toml`
- Modify: `rust/crates/brain-core/src/tool_registry.rs`
- Modify: `rust/crates/brain-core/src/guard_check.rs`
- Modify: `rust/crates/brain-core/src/types.rs`
- Modify: `rust/crates/brain-core/src/lib.rs`
- Modify: `rust/Cargo.lock`

- [ ] **Step 1: 写策略顺序 RED 测试**

```rust
#[test]
fn policy_denies_permission_before_parameter_guard() {
    let registration = registration_with_permission(
        "plugin__danger__run",
        ToolPermission::DangerFullAccess,
        true,
    );
    let call = call("plugin__danger__run", serde_json::json!({}));
    assert_eq!(
        ToolAuthorizationPolicy::read_only().decide(
            snapshot_with(&registration).fingerprint(), &registration, &call, &ToolApproval::None,
        ),
        ToolPolicyDecision::Deny("工具需要 DangerFullAccess，当前最多允许 ReadOnly".into()),
    );
}

#[test]
fn unknown_calls_are_denied_before_route_execution() {
    let view = ToolRequestView::empty(Arc::new(ToolSnapshot::empty()));
    assert_eq!(authorize_call(&view, &ToolAuthorizationPolicy::default(), &call("missing", json!({})), &ToolApproval::None),
               ToolPolicyDecision::Deny("工具 missing 未在本次请求中声明".into()));
}

#[test]
fn destructive_builtin_parameter_requires_confirmation() {
    let registration = registration_with_permission("bash", ToolPermission::DangerFullAccess, false);
    let call = call("bash", json!({"command": "git reset --hard"}));
    assert!(matches!(
        ToolAuthorizationPolicy::default().decide(
            snapshot_with(&registration).fingerprint(), &registration, &call, &ToolApproval::None,
        ),
        ToolPolicyDecision::RequireConfirmation(_)
    ));
}

#[test]
fn destructive_command_alias_uses_canonical_guard_profile() {
    let registration = registration_with_alias_and_guard(
        "bash", "shell", ToolGuardProfile::ShellCommand,
    );
    let call = call("shell", json!({"command": "git reset --hard"}));
    assert!(matches!(
        ToolAuthorizationPolicy::default().decide(
            snapshot_with(&registration).fingerprint(), &registration, &call, &ToolApproval::None,
        ),
        ToolPolicyDecision::RequireConfirmation(_)
    ));
}

#[test]
fn trusted_confirmation_satisfies_confirmation_but_not_permission_ceiling() {
    let registration = registration_with_permission(
        "mcp__remote__write", ToolPermission::DangerFullAccess, true,
    );
    let call = call("mcp__remote__write", json!({}));
    let snapshot = snapshot_with(&registration);
    let approval = ToolApproval::UserConfirmed {
        call_id: "toolu_1".into(),
        fingerprint: tool_call_fingerprint(
            "toolu_1", snapshot.fingerprint(), &registration, &call,
        ),
    };
    assert_eq!(
        ToolAuthorizationPolicy::default().decide(
            snapshot.fingerprint(), &registration, &call, &approval,
        ),
        ToolPolicyDecision::Allow,
    );
    assert!(matches!(
        ToolAuthorizationPolicy::read_only().decide(
            snapshot.fingerprint(), &registration, &call, &approval,
        ),
        ToolPolicyDecision::Deny(_)
    ));
}

#[test]
fn confirmation_cannot_be_reused_after_arguments_change() {
    let registration = registration_with_permission("bash", ToolPermission::DangerFullAccess, true);
    let approved_call = call("bash", json!({"command":"cargo test"}));
    let changed_call = call("bash", json!({"command":"git reset --hard"}));
    let snapshot = snapshot_with(&registration);
    let approval = ToolApproval::UserConfirmed {
        call_id: "toolu_2".into(),
        fingerprint: tool_call_fingerprint(
            "toolu_2", snapshot.fingerprint(), &registration, &approved_call,
        ),
    };
    assert!(matches!(
        ToolAuthorizationPolicy::default().decide(
            snapshot.fingerprint(), &registration, &changed_call, &approval,
        ),
        ToolPolicyDecision::RequireConfirmation(_)
    ));
}

#[test]
fn hard_parameter_guard_cannot_be_overridden_by_confirmation() {
    let registration = registration_with_alias_and_guard(
        "WebFetch", "fetch", ToolGuardProfile::RemoteUrl,
    );
    let call = call("fetch", json!({"url":"http://127.0.0.1/admin"}));
    let snapshot = snapshot_with(&registration);
    let approval = ToolApproval::UserConfirmed {
        call_id: "toolu_3".into(),
        fingerprint: tool_call_fingerprint(
            "toolu_3", snapshot.fingerprint(), &registration, &call,
        ),
    };
    assert!(matches!(
        ToolAuthorizationPolicy::default().decide(
            snapshot.fingerprint(), &registration, &call, &approval,
        ),
        ToolPolicyDecision::Deny(_)
    ));
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-core tool_policy --lib -- --nocapture`

Expected: 编译失败，新策略模块不存在。

- [ ] **Step 3: 增加 URL 解析依赖并实现显式三态策略**

在 `brain-core/Cargo.toml` 增加 `url = "2"`，用于 `RemoteUrl` 的语法、scheme、userinfo、host/IP 检查；不得用字符串前缀代替 URL parser。

```rust
use crate::guard_check::{guard_check_profile, GuardResult};
use crate::tool_registry::{ToolPermission, ToolRegistration, ToolRequestView};
use crate::types::ToolCall;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPolicyDecision {
    Allow,
    Deny(String),
    RequireConfirmation(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolApproval {
    None,
    UserConfirmed { call_id: String, fingerprint: [u8; 32] },
}

#[derive(Debug, Clone)]
pub struct ToolAuthorizationPolicy {
    max_permission: ToolPermission,
}

impl Default for ToolAuthorizationPolicy {
    fn default() -> Self { Self { max_permission: ToolPermission::DangerFullAccess } }
}

impl ToolAuthorizationPolicy {
    pub fn read_only() -> Self { Self { max_permission: ToolPermission::ReadOnly } }

    pub fn decide(
        &self,
        snapshot_fingerprint: [u8; 32],
        registration: &ToolRegistration,
        call: &ToolCall,
        approval: &ToolApproval,
    ) -> ToolPolicyDecision {
        if registration.metadata.permission > self.max_permission {
            return ToolPolicyDecision::Deny(format!(
                "工具需要 {:?}，当前最多允许 {:?}",
                registration.metadata.permission, self.max_permission,
            ));
        }
        let guard_reason = match guard_check_profile(registration.metadata.guard_profile, call) {
            GuardResult::Pass => None,
            GuardResult::NeedUserConfirm { reason } => Some(reason),
            GuardResult::Deny { reason } => return ToolPolicyDecision::Deny(reason),
        };
        let confirmation_reason = guard_reason.or_else(|| {
            registration.metadata.requires_confirmation.then(|| {
                format!("工具 {} 的 Provider 要求用户确认", registration.canonical_name)
            })
        });
        match (confirmation_reason, approval) {
            (None, _) => ToolPolicyDecision::Allow,
            (Some(_), ToolApproval::UserConfirmed { call_id, fingerprint })
                if *fingerprint == tool_call_fingerprint(
                    call_id, snapshot_fingerprint, registration, call,
                ) => {
                    ToolPolicyDecision::Allow
                }
            (Some(reason), _) => ToolPolicyDecision::RequireConfirmation(reason),
        }
    }
}

pub fn tool_call_fingerprint(
    call_id: &str,
    snapshot_fingerprint: [u8; 32],
    registration: &ToolRegistration,
    call: &ToolCall,
) -> [u8; 32] {
    let canonical_input = canonical_json_bytes(&call.input);
    let mut hasher = sha2::Sha256::new();
    for field in [call_id.as_bytes(), snapshot_fingerprint.as_slice(),
                  registration.provider_id.as_bytes(), registration.canonical_name.as_bytes(),
                  canonical_input.as_slice()] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    hasher.finalize().into()
}

pub fn authorize_call(
    view: &ToolRequestView,
    policy: &ToolAuthorizationPolicy,
    call: &ToolCall,
    approval: &ToolApproval,
) -> ToolPolicyDecision {
    match view.resolve_advertised(&call.tool_name) {
        Some(registration) => policy.decide(
            view.snapshot().fingerprint(), &registration, call, approval,
        ),
        None => ToolPolicyDecision::Deny(format!("工具 {} 未在本次请求中声明", call.tool_name)),
    }
}
```

为调用 `Sha256::new/update/finalize` 导入 `sha2::Digest`。测试中传入 `snapshot_with(&registration).fingerprint()`；tool loop 传 `request_view.snapshot().fingerprint()`。`canonical_json_bytes` 是私有且无失败分支：object key 递归排序，array 保序，string/bool/null/number 用 serde_json 的规范转义/表示。长度前缀避免字段拼接二义性。approval 由 tool loop 创建、复核一次后立即 drop，只能用于完全相同的 snapshot、call_id、canonical/provider 和参数；不得缓存到下一轮或请求。

在 `tool_registry.rs` 定义并序列化 `ToolGuardProfile { None, ShellCommand, SensitivePath, RemoteUrl }`，加入 `ToolMetadata.guard_profile`；`ToolMetadata::read_only` 默认为 `None`。把 `GuardResult` 扩为 `Pass | NeedUserConfirm { reason } | Deny { reason }`。在 `guard_check.rs` 增加 `guard_check_profile(profile, call)`，直接读取对应参数字段，不再根据 `call.tool_name` 判断；旧 `guard_check(call)` 仅作为兼容 wrapper，把旧 canonical 名映射到 profile。这样 `bash` 的显式 alias 也不能绕过危险命令检查。`RemoteUrl` 对非 http(s)、含凭据 URL、loopback/link-local/私网字面目标返回硬 `Deny`，即使用户确认也不能覆盖；Web 客户端在 DNS/redirect 阶段再次验证解析结果和跨域/降级跳转，避免仅靠字符串检查。参数 guard 必须始终执行，不能因 Provider 已设置 `requires_confirmation` 而跳过。

在 `types.rs` 增加 `ToolApprovalSender(oneshot::Sender<bool>)` 与 `ProgressEvent::ConfirmTool { call_id, tool_name, provider_id, reason, input_preview, response_tx }`。`Debug` 只显示 sender 类型；`input_preview` 使用统一脱敏器并限长，不能带 token/header。`ToolApproval` 不从 JSON 反序列化，也不存进 `ToolCall`；模型生成的 `validated/validation_id` 字段一律覆盖为 false/None。只有本地交互通道同意后，宿主才在内存中构造带当次 call fingerprint 的 `ToolApproval::UserConfirmed`；为兼容旧 route 可在策略复核通过后把传给 route 的 clone 标记为 validated，但策略本身永远不信任该字段。call_id 相同但参数、canonical/provider/generation 不同必须再次确认。

- [ ] **Step 4: 运行 GREEN 并提交**

Run: `cargo test -p brain-core tool_policy --lib -- --nocapture`

Expected: 7 个策略测试通过，alias guard、硬拒绝、显式确认与确认不可复用均有覆盖。

```powershell
git add crates/brain-core/Cargo.toml crates/brain-core/src/guard_check.rs crates/brain-core/src/lib.rs crates/brain-core/src/tool_policy.rs crates/brain-core/src/tool_registry.rs crates/brain-core/src/types.rs Cargo.lock
git commit -m "feat(core): enforce dynamic tool policy"
```

### Task 5: MainBrain 在请求入口固定 snapshot

**Files:**
- Modify: `rust/crates/brain-main/src/main_brain.rs`
- Modify: `rust/crates/brain-main/Cargo.toml`

- [ ] **Step 1: 写同步、流式和压缩重入的 RED 测试**

增加一个 `RecordingToolsLlm`，第一次调用记录 request tools 并阻塞在 `Notify`，第二次返回文本。测试流程：

```rust
#[tokio::test]
async fn in_flight_request_keeps_pinned_snapshot_while_next_request_sees_refresh() {
    let registry = Arc::new(DynamicToolRegistry::new());
    registry.replace_provider(contribution("builtin", vec!["old_tool"])).unwrap();
    let llm = Arc::new(RecordingToolsLlm::new());
    let mut first = MainBrain::new_with_registry(
        llm.clone(), registry.clone(), BrainConfig::default(), 4096, 0.0,
    );
    let started = llm.started.clone();
    let release = llm.release.clone();
    let first_task = tokio::spawn(async move { first.process_input("first", None, None).await });
    started.notified().await;
    registry.replace_provider(contribution("builtin", vec!["new_tool"])).unwrap();
    release.notify_waiters();
    first_task.await.unwrap().unwrap();

    let mut second = MainBrain::new_with_registry(
        llm.clone(), registry, BrainConfig::default(), 4096, 0.0,
    );
    second.process_input("second", None, None).await.unwrap();
    assert_eq!(llm.seen_tool_names(), vec![vec!["old_tool"], vec!["new_tool"]]);
}
```

再为 `process_input_streaming` 写同样的 refresh 屏障，并把现有 context overflow fixture 改为断言两次 LLM call 的工具名完全相同。

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-main pinned_snapshot --lib -- --nocapture`

Expected: 编译失败，`new_with_registry` 不存在，MainBrain 仍持有 `Vec<ToolDefinition>`。

- [ ] **Step 3: 替换 MainBrain 工具字段与动态构造器**

把字段改为：

```rust
tool_registry: Arc<dyn ToolRegistryView>,
legacy_registry: Option<Arc<StaticToolRegistry>>,
legacy_executor: Option<Arc<dyn ToolExecutor>>,
session_contribution: Option<ProviderContribution>,
tool_policy: ToolAuthorizationPolicy,
```

增加构造器：

```rust
pub fn new_with_registry(
    llm: Arc<dyn LlmProvider>,
    tool_registry: Arc<dyn ToolRegistryView>,
    config: BrainConfig,
    llm_max_tokens: u32,
    llm_temperature: f64,
) -> Self {
    Self::new_with_registry_in_context(
        llm,
        tool_registry,
        config,
        llm_max_tokens,
        llm_temperature,
        ToolExecutionContext::default(),
    )
}

fn request_view(&self) -> Result<ToolRequestView> {
    let base = self.tool_registry.snapshot();
    let snapshot = match &self.session_contribution {
        Some(session) => base.derive_session(session.clone())
            .map_err(|error| MainBrainError::ToolRegistry(error.to_string()))?,
        None => base,
    };
    Ok(ToolRequestView::base(snapshot))
}
```

`new_with_registry_in_context` 初始化 history/阈值/usage 的代码复用现有构造器私有 helper，不复制两份。

- [ ] **Step 4: 把旧构造器改成 StaticToolRegistry 适配器**

先在 `brain-core/src/tool_registry.rs` 增加完整静态适配器：

```rust
pub struct StaticToolRegistry {
    provider_id: String,
    current: RwLock<Arc<ToolSnapshot>>,
}

impl StaticToolRegistry {
    pub fn new(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            current: RwLock::new(Arc::new(ToolSnapshot::empty())),
        }
    }

    pub fn replace(
        &self,
        registrations: Vec<Arc<ToolRegistration>>,
    ) -> Result<Arc<ToolSnapshot>, ToolRegistryError> {
        if registrations.iter().any(|item| item.provider_id != self.provider_id) {
            return Err(ToolRegistryError::ProviderMismatch(self.provider_id.clone()));
        }
        let mut current = self.current.write().map_err(|_| ToolRegistryError::Poisoned)?;
        let version = current.version().checked_add(1)
            .ok_or(ToolRegistryError::VersionOverflow)?;
        let contribution = ProviderContribution::ready(self.provider_id.clone(), registrations);
        let snapshot = Arc::new(ToolSnapshot::build(
            version,
            BTreeMap::from([(self.provider_id.clone(), contribution)]),
        )?);
        *current = Arc::clone(&snapshot);
        Ok(snapshot)
    }
}

impl ToolRegistryView for StaticToolRegistry {
    fn snapshot(&self) -> Arc<ToolSnapshot> {
        match self.current.read() {
            Ok(snapshot) => Arc::clone(&snapshot),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }
}
```

从读取旧 version 到提交新 snapshot 始终持有同一把 write lock，避免并发兼容调用丢失版本。为 `StaticToolRegistry` 增加并发替换、一次替换和 provider mismatch 测试。旧 `MainBrain::new/new_in_context` 创建 `StaticToolRegistry::new("session:legacy-tests")`，保存 legacy executor。`register_tools` 使用传入 definition 构建 registration：

```rust
pub fn register_tools(&mut self, tools: Vec<ToolDefinition>) {
    let Some(registry) = &self.legacy_registry else {
        tracing::warn!("动态 MainBrain 忽略旧 register_tools；请使用 SessionProvider");
        return;
    };
    let Some(executor) = &self.legacy_executor else { return; };
    let registrations = tools.into_iter().map(|tool| {
        Arc::new(ToolRegistration::new(
            "session:legacy-tests",
            ToolSourceKind::Session,
            tool.name.clone(),
            ToolDescriptor {
                name: tool.name.clone(),
                description: tool.description,
                input_schema: tool.input_schema,
            },
            ToolMetadata::read_only(ToolExposure::Base),
            Arc::new(ExecutorToolRoute::new(tool.name, Arc::clone(executor))),
        ).expect("测试工具定义必须有效"))
    }).collect();
    registry.replace(registrations).expect("测试工具快照替换失败");
}
```

这里的 provider ID 必须与 `StaticToolRegistry` contribution 一致，避免 ProviderMismatch。

- [ ] **Step 5: 在同步和流式入口只捕获一次 RequestView**

`process_input` 在构建 messages 前执行：

```rust
let mut request_view = self.request_view()?;
let messages = self.build_messages(&request_view);
```

上下文溢出 loop 始终传 `&mut request_view`，不重新读取 registry。`process_input_streaming` 在 `tokio::spawn` 前捕获 `request_view` 并 move 进去。`build_messages` 用 `request_view.descriptors().is_empty()` 选择 prompt，并按 `ToolPromptKind::Bootstrap`、核心规则、环境、`SkillsSummary` 的固定顺序加入 `request_view.snapshot().prompt_fragments()`。

- [ ] **Step 6: fork 只复制 registry/policy/session 描述，不固定全局 snapshot**

隔离 fork 复制 `Arc<dyn ToolRegistryView>`、policy 和可选 session contribution；它在自己的 `process_input` 开始时再取一次全局 snapshot。旧 additional definitions 通过一个 `session:legacy-fork` contribution 和传入 executor route 适配，禁止再 clone/extend `Vec<ToolDefinition>`。

- [ ] **Step 7: 运行 GREEN 并提交**

Run:

```powershell
cargo test -p brain-main pinned_snapshot --lib -- --nocapture
cargo test -p brain-main streaming_main_brain --lib -- --nocapture
cargo test -p brain-main context_overflow --lib -- --nocapture
```

Expected: 同步、流式和重入测试通过，旧构造测试保持通过。

```powershell
git add crates/brain-main/Cargo.toml crates/brain-main/src/main_brain.rs crates/brain-core/src/tool_registry.rs
git commit -m "feat(main): pin tool snapshots per request"
```

### Task 6: tool loop 从同一 RequestView 广告、授权和执行 route

**Files:**
- Modify: `rust/crates/brain-main/src/tool_loop.rs`
- Modify: `rust/crates/ai-brain-cli/src/terminal.rs`
- Modify: `rust/crates/ai-brain-cli/src/tui/app.rs`
- Modify: `rust/crates/ai-brain-cli/src/tui/output.rs`
- Modify: `rust/crates/ai-brain-cli/src/tui/session_logger.rs`
- Modify: `rust/crates/ai-brain-cli/src/main.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/progress_adapter.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/ws_handler.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/static/app.js`

- [ ] **Step 1: 写 route 一致性、未知拒绝和 hook RED 测试**

新增测试：

```rust
#[tokio::test]
async fn advertised_tool_executes_route_from_the_same_snapshot() {
    let route = Arc::new(RecordingRoute::new("snapshot-route"));
    let mut view = request_view_with_route("context_probe", route.clone());
    let result = run_tool_loop_with_view(
        &ToolCallingLlm::new(),
        &ToolExecutionContext::new("/tmp/request-view"),
        &mut vec![ChatMessage::user("调用工具")],
        &mut view,
        &ToolAuthorizationPolicy::default(),
        None,
        None,
        4096,
        0.0,
        None,
    ).await.unwrap();
    assert_eq!(result.response.text(), "工具调用完成");
    assert_eq!(route.calls(), 1);
}

#[tokio::test]
async fn unadvertised_tool_is_rejected_without_route_or_ask_user_side_effects() {
    let llm = ForcedToolLlm::new("AskUserQuestion");
    let mut view = ToolRequestView::empty(Arc::new(ToolSnapshot::empty()));
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    run_tool_loop_with_view(
        &llm, &ToolExecutionContext::default(), &mut vec![ChatMessage::user("x")],
        &mut view, &ToolAuthorizationPolicy::default(), Some(&tx), None, 4096, 0.0, None,
    ).await.unwrap();
    assert!(!matches!(rx.try_recv(), Ok(ProgressEvent::AskUser { .. })));
}
```

给现有 HookRunner fixture 增加成功 route 触发 PostToolUse、error result 触发 PostToolUseFailure、policy deny 两者都不触发的断言。

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-main advertised_tool_executes_route_from_the_same_snapshot --lib -- --nocapture`

Expected: 当前函数仍要求独立 executor/tools，测试无法编译。

- [ ] **Step 3: 新增唯一动态主循环入口**

把内部入口签名固定为：

```rust
pub async fn run_tool_loop_with_view(
    provider: &dyn LlmProvider,
    tool_execution_context: &ToolExecutionContext,
    messages: &mut Vec<ChatMessage>,
    request_view: &mut ToolRequestView,
    policy: &ToolAuthorizationPolicy,
    progress_tx: Option<&tokio::sync::mpsc::Sender<ProgressEvent>>,
    hook_runner: Option<&HookRunner>,
    max_tokens: u32,
    temperature: f64,
    cancel: Option<CancellationToken>,
) -> Result<ToolLoopResult>;
```

每轮请求从 `request_view.descriptors()` 映射：

```rust
let definitions = request_view.descriptors().into_iter().map(|tool| ToolDefinition {
    name: tool.name,
    description: tool.description,
    input_schema: tool.input_schema,
}).collect::<Vec<_>>();
let request = build_request(messages, &definitions, max_tokens, temperature);
```

删除动态入口的 `tool_executor` 参数。旧公开 wrappers 仅在测试兼容层创建 `StaticToolRegistry + ToolRequestView` 后调用此函数。

- [ ] **Step 4: 按固定顺序重写单个调用执行**

每个 ToolUse 依次执行以下代码路径：

```rust
let Some(registration) = request_view.resolve_advertised(name) else {
    append_denied_tool_result(id, name, input, "未在本次请求中声明", messages, turns, progress_tx).await;
    continue;
};
let mut tool_call = ToolCall {
    tool_name: name.clone(),
    input: input.clone(),
    validated: false,
    validation_id: None,
};
match policy.decide(
    request_view.snapshot().fingerprint(), &registration, &tool_call, &ToolApproval::None,
) {
    ToolPolicyDecision::Allow => {}
    ToolPolicyDecision::Deny(reason) => {
        append_denied_tool_result(id, name, input, &reason, messages, turns, progress_tx).await;
        continue;
    }
    ToolPolicyDecision::RequireConfirmation(reason) => {
        let approved = request_tool_confirmation(
            progress_tx, id, &registration, input, &reason,
        ).await;
        if !approved {
            append_denied_tool_result(
                id, name, input,
                &format!("工具未获用户确认: {reason}"),
                messages, turns, progress_tx,
            ).await;
            continue;
        }
        let approval = ToolApproval::UserConfirmed {
            call_id: id.clone(),
            fingerprint: tool_call_fingerprint(
                &id, request_view.snapshot().fingerprint(), &registration, &tool_call,
            ),
        };
        if !matches!(
            policy.decide(
                request_view.snapshot().fingerprint(), &registration, &tool_call, &approval,
            ),
            ToolPolicyDecision::Allow
        ) {
            append_denied_tool_result(
                id, name, input, "确认凭据未通过策略复核", messages, turns, progress_tx,
            ).await;
            continue;
        }
        tool_call.validated = true;
        tool_call.validation_id = Some(format!("user:{id}"));
    }
}
```

`request_tool_confirmation` 只有在 `progress_tx` 存在时发送 `ProgressEvent::ConfirmTool` 并等待 `bool` oneshot；通道不存在、receiver 丢失、取消或等待超过配置的 5 分钟上限均返回 false。等待确认前不得运行 PreToolUse/route。补充测试：确认 yes 后 route 恰好执行一次且收到 `validated=true`；确认 no、超时、无进度通道和伪造模型 `validated=true` 均不执行 route（LLM 入站字段必须被覆盖为 false）。

接通所有生产交互面：terminal/TUI 显示工具名、Provider、原因和脱敏输入并只接受明确 yes；非交互 `V2Test`/管道模式在 `main.rs` 收到 ConfirmTool 时立即发送 false，绝不挂住。Web 增加 `WebProgressEvent::ConfirmTool` 与 `ClientMessage::ToolApproval { call_id, approved }`，`ws_handler` 用 `BTreeMap<call_id, ToolApprovalSender>` 保存当前连接 pending approval，响应后立即移除，重复/未知 call_id 返回错误；`web/static/app.js` 显示明确“允许一次/拒绝”按钮并回传 call_id。连接关闭、取消查询或查询结束时向所有 pending sender 发送 false。协作成员没有专属审批归属时默认拒绝，不把批准广播给房间其他成员。现有 `AskResponse` 不复用，避免普通问答被当成安全批准。

然后运行 PreToolUse hook。`ToolRouteKind::AskUser` 才进入现有 oneshot AskUser 分支；禁止再用 `name == "AskUserQuestion"` 判断。`ToolRouteKind::Backend` 调：

```rust
let result = match registration.route.execute(&tool_call, tool_execution_context).await {
    Ok(result) => result,
    Err(error) => ToolExecutionResult {
        tool_name: name.clone(),
        output: format!("{:?}: {}", error.kind, error.message),
        is_error: true,
        duration_ms: 0,
    },
};
```

成功调用 `HookEvent::PostToolUse`，`result.is_error` 调 `HookEvent::PostToolUseFailure`。未知/策略/PreToolUse 拒绝不调用 post hook。

- [ ] **Step 5: 保持上下文、截断、进度与 turn 语义**

保留现有 50K 输出截断、ToolStart/ToolDone/IntermediateConclusion、`ToolCallRecord` 和取消检查。实际 `duration_ms` 优先使用 route result 中的非零值，否则使用 tool loop 实测值；日志使用 public name，但诊断字段另带 canonical/provider ID。

- [ ] **Step 6: 运行 GREEN 和全 brain-main 回归**

Run:

```powershell
cargo test -p brain-main advertised_tool_executes_route_from_the_same_snapshot --lib -- --nocapture
cargo test -p brain-main unadvertised_tool_is_rejected_without_route_or_ask_user_side_effects --lib -- --nocapture
cargo test -p brain-main --lib
cargo test -p ai-brain-cli tool_approval --lib -- --nocapture
```

Expected: 全部通过。

- [ ] **Step 7: 提交**

```powershell
git add crates/brain-main/src/tool_loop.rs crates/brain-main/src/main_brain.rs crates/ai-brain-cli/src/main.rs crates/ai-brain-cli/src/terminal.rs crates/ai-brain-cli/src/tui/app.rs crates/ai-brain-cli/src/tui/output.rs crates/ai-brain-cli/src/tui/session_logger.rs crates/ai-brain-cli/src/web/progress_adapter.rs crates/ai-brain-cli/src/web/ws_handler.rs crates/ai-brain-cli/src/web/static/app.js
git commit -m "feat(main): route tools through request snapshots"
```

### Task 7: 让 ToolSearch 激活当前快照中的 deferred 工具

**Files:**
- Modify: `rust/crates/brain-main/src/tool_loop.rs`
- Modify: `rust/crates/brain-core/src/tool_registry.rs`

- [ ] **Step 1: 写动态 deferred MCP/插件 RED 测试**

LLM 第一次调用 `ToolSearch {"query":"select:mcp__docs__search,plugin__lint__run"}`，第二次调用 `mcp__docs__search`，第三次返回最终文本。snapshot 含一个 base ToolSearch 和两个 deferred route：

```rust
#[tokio::test]
async fn tool_search_activates_deferred_tools_from_the_pinned_snapshot() {
    let mcp_route = Arc::new(RecordingRoute::new("mcp-result"));
    let plugin_route = Arc::new(RecordingRoute::new("plugin-result"));
    let mut view = request_view(vec![
        system_registration("ToolSearch", ToolExposure::Base, ToolRouteKind::ToolSearch),
        routed_registration("mcp__docs__search", ToolExposure::Deferred, mcp_route.clone()),
        routed_registration("plugin__lint__run", ToolExposure::Deferred, plugin_route),
    ]);
    run_tool_loop_with_view(
        &SearchThenCallLlm::new(), &ToolExecutionContext::default(),
        &mut vec![ChatMessage::user("search")], &mut view,
        &ToolAuthorizationPolicy::default(), None, None, 4096, 0.0, None,
    ).await.unwrap();
    assert!(view.resolve_advertised("mcp__docs__search").is_some());
    assert_eq!(mcp_route.calls(), 1);
}
```

并在第一次 ToolSearch 与 registry refresh 之间插入屏障，断言新加入全局 registry 的 `mcp__new__tool` 不会被本请求搜到。

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-main tool_search_activates_deferred_tools_from_the_pinned_snapshot --lib -- --nocapture`

Expected: ToolSearch 系统 route 尚未实现，测试失败。

- [ ] **Step 3: 实现兼容查询与选择解析**

在 tool loop 增加纯函数：

```rust
#[derive(serde::Deserialize)]
struct ToolSearchInput { query: String, max_results: Option<usize> }

#[derive(serde::Serialize)]
struct ToolSearchMatch {
    name: String,
    description: String,
    provider_id: String,
    source_kind: ToolSourceKind,
    permission: ToolPermission,
    risk_level: ToolRiskLevel,
}

#[derive(serde::Serialize)]
struct ToolSearchOutput {
    matches: Vec<String>,
    query: String,
    normalized_query: String,
    total_deferred_tools: usize,
    pending_mcp_servers: Option<Vec<String>>,
    tools: Vec<ToolSearchMatch>,
    activated: bool,
}

fn execute_tool_search(
    view: &mut ToolRequestView,
    input: &serde_json::Value,
) -> Result<ToolExecutionResult, String> {
    let input: ToolSearchInput = serde_json::from_value(input.clone())
        .map_err(|error| format!("ToolSearch 参数错误: {error}"))?;
    let limit = input.max_results.unwrap_or(5).clamp(1, 20);
    let query = input.query.trim().to_string();
    let normalized_query = normalize_tool_search_query(&query);
    let selection = query.strip_prefix("select:").map(str::trim);
    let total_deferred_tools = view.snapshot().registrations().iter()
        .filter(|item| item.metadata.exposure == ToolExposure::Deferred).count();
    let matches = if let Some(selection) = selection {
        let names = selection.split(',').map(str::trim)
            .filter(|name| !name.is_empty()).map(str::to_string).collect::<Vec<_>>();
        view.activate_deferred(&names).map_err(|error| error.to_string())?;
        names.into_iter().filter_map(|name| view.snapshot().resolve(&name)).collect::<Vec<_>>()
    } else {
        view.deferred_matches(input.query.trim(), limit)
    };
    let output = matches.iter().map(|item| ToolSearchMatch {
        name: item.canonical_name.clone(),
        description: item.descriptor.description.clone(),
        provider_id: item.provider_id.clone(),
        source_kind: item.source_kind,
        permission: item.metadata.permission,
        risk_level: item.metadata.risk_level,
    }).collect::<Vec<_>>();
    let compatible_matches = output.iter().map(|item| item.name.clone()).collect();
    Ok(ToolExecutionResult {
        tool_name: "ToolSearch".into(),
        output: serde_json::to_string_pretty(&ToolSearchOutput {
            matches: compatible_matches,
            query,
            normalized_query,
            total_deferred_tools,
            pending_mcp_servers: None,
            tools: output,
            activated: selection.is_some(),
        }).map_err(|error| error.to_string())?,
        is_error: false,
        duration_ms: 0,
    })
}
```

`normalize_tool_search_query` 从旧 tools 实现迁移为同文件纯函数，保留现有 alias/keyword 兼容测试。输出继续保留旧 `matches/query/normalized_query/total_deferred_tools/pending_mcp_servers` 字段，同时增加结构化 `tools/activated`；`ToolRouteKind::ToolSearch` 调此函数，选择结果只改变传入 view 的 advertised set。

- [ ] **Step 4: 运行 GREEN 和请求固定测试**

Run:

```powershell
cargo test -p brain-main tool_search_activates_deferred_tools_from_the_pinned_snapshot --lib -- --nocapture
cargo test -p brain-main tool_search_does_not_see_registry_refresh_mid_request --lib -- --nocapture
cargo test -p brain-main --lib
```

Expected: 动态 MCP/插件 deferred 测试通过，旧快照固定不变。

- [ ] **Step 5: 提交**

```powershell
git add crates/brain-main/src/tool_loop.rs crates/brain-core/src/tool_registry.rs
git commit -m "feat(main): activate deferred tools dynamically"
```

### Task 8: 阶段门禁与静态适配器证明

**Files:**
- Modify: `rust/crates/brain-main/src/main_brain.rs`（仅在阶段测试暴露缺口时）
- Modify: `rust/crates/brain-core/src/tool_registry.rs`（仅在阶段测试暴露缺口时）

- [ ] **Step 1: 写 adapter 不产生第二份 truth 的合同测试**

```rust
#[tokio::test]
async fn legacy_register_tools_builds_routes_in_the_same_static_snapshot() {
    let executor = Arc::new(StubToolExecutor::new().with_response("read_file", "contents".into()));
    let mut brain = MainBrain::new(TextAfterToolLlm::arc("read_file"), executor, config(), 4096, 0.0);
    brain.register_tools(vec![test_tool_definition("read_file")]);
    let view = brain.request_view().unwrap();
    let registration = view.resolve_advertised("read_file").unwrap();
    assert_eq!(registration.descriptor.name, "read_file");
    assert_eq!(registration.route.kind(), ToolRouteKind::Backend);
    assert_eq!(brain.process_input("read", None, None).await.unwrap().answer, "done");
}
```

- [ ] **Step 2: 运行阶段测试**

Run:

```powershell
cargo test -p brain-core
cargo test -p brain-motor
cargo test -p brain-main
cargo fmt --check
cargo clippy -p brain-core -p brain-motor -p brain-main --all-targets -- -D warnings
```

Expected: 全部退出 0。

- [ ] **Step 3: 扫描旧真相字段**

Run:

```powershell
rg -n -F -e 'tools: Vec<ToolDefinition>' -e '&self.tools' crates/brain-main/src
rg -n -F -e 'with_builtin_tools' -e 'execute_stub' crates/brain-motor/src
```

Expected: 两条命令均无生产命中；只允许测试函数名中出现 legacy adapter 文案。

- [ ] **Step 4: 提交阶段测试（若有改动）**

```powershell
git add crates/brain-core/src/tool_registry.rs crates/brain-core/src/tool_policy.rs crates/brain-main/src/main_brain.rs crates/brain-motor/src/tool_registry.rs
git diff --cached --check
git commit -m "test(tools): lock request snapshot contracts"
```

若 Step 2–3 没有产生文件变化，则不创建空提交。
