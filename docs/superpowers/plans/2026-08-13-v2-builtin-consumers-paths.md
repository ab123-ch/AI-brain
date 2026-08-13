# 智脑 v2 Builtin、消费者、路径与权限 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让内置能力按真实依赖和平台动态发布，并让 Motor、Eval、协作会话与 v2 生产组装只消费共享工具快照，同时彻底消除 Windows 用户目录和 shell 探测的假设。

**Architecture:** `AiBrainPaths` 负责所有智脑用户/项目路径，`runtime::ShellBackend` 负责可执行 shell 探测；`tools::BuiltinProvider` 根据一个不可变 `BuiltinCapabilities` 生成带真实 route 的注册记录。Orchestrator 创建空 `DynamicToolRegistry` 后将它注入 Motor/MainBrain，再由 `ToolRuntime` 首次发布 builtin contribution；Eval 和协作运行只派生受限 RequestView，不再维护另一份工具表。

**Tech Stack:** Rust 2021、Tokio、serde/serde_json、现有 brain-core/brain-motor/brain-main/tools/runtime/brain-eval/ai-brain-cli crates。

---

## 文件结构

- Create: `rust/crates/brain-core/src/paths.rs`
- Modify: `rust/crates/brain-core/src/lib.rs`
- Modify: `rust/crates/brain-core/src/config.rs`
- Modify: `rust/crates/runtime/src/bash.rs`
- Modify: `rust/crates/runtime/src/lib.rs`
- Create: `rust/crates/tools/src/provider.rs`
- Modify: `rust/crates/tools/src/lib.rs`
- Modify: `rust/crates/tools/Cargo.toml`
- Modify: `rust/crates/ai-brain-cli/src/real_tool_executor.rs`
- Create: `rust/crates/ai-brain-cli/src/tool_runtime.rs`
- Modify: `rust/crates/ai-brain-cli/src/lib.rs`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`
- Modify: `rust/crates/ai-brain-cli/src/config_manager.rs`
- Modify: `rust/crates/ai-brain-cli/src/init.rs`
- Modify: `rust/crates/brain-motor/src/motor_brain.rs`
- Modify: `rust/crates/brain-motor/src/error.rs`
- Modify: `rust/crates/brain-eval/src/eval_brain.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration_tools.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs`
- Modify: `rust/crates/brain-integration-tests/Cargo.toml`
- Create: `rust/crates/brain-integration-tests/tests/dynamic_tools.rs`
- Modify: affected `Cargo.toml` files only where the dependency graph requires it

## 固定能力矩阵

`BuiltinProvider` 只能按下表发布。Schema 仍可由静态 catalog 定义；“是否存在”必须来自运行时能力探测。

| 工具 | 发布条件 | exposure | route |
|---|---|---|---|
| `read_file`、`glob_search`、`grep_search` | workspace 已规范化 | base | `RealToolExecutor` |
| `write_file`、`edit_file` | workspace 已规范化且策略允许写 | base | `RealToolExecutor` |
| `bash` | 找到可执行 `sh` | base | 注入所选 `ShellBackend::Sh` |
| `PowerShell` | 找到 `pwsh` 或 `powershell.exe` | deferred | 注入所选 PowerShell backend |
| `ToolSearch` | 始终存在 | base | `SystemToolRoute(ToolSearch)` |
| `AskUserQuestion` | progress/user-response channel 可用 | deferred | `SystemToolRoute(AskUserQuestion)` |
| `TodoWrite`、`NotebookEdit`、`Sleep`、`SendUserMessage`、`Config`、`EnterPlanMode`、`ExitPlanMode`、`StructuredOutput`、`REPL` | 对应真实实现通过 capability flag | deferred | `RealToolExecutor` |
| `Skill` | catalog 非空 | deferred | catalog snapshot route |
| `Agent` | agent runtime/dispatch 可用 | base | agent route |
| `search_memory`、`list_recent_memories` | memory handle 存在 | deferred | memory route |
| graph 九个工具 | graph DB 已成功打开 | deferred | graph route |
| `WebSearch`、`WebFetch` | 子计划 D 发布 | 不在本阶段发布 | search provider |
| MCP 原生工具和资源门面 | 子计划 C 发布 | 不在本阶段发布 | MCP provider |
| Plugin 工具 | 子计划 C 发布 | 不在本阶段发布 | plugin provider |
| `LSP`、`RemoteTrigger`、`MCP` | 无真实后端 | 不发布 | 无 |
| `TestingPermission` | 仅测试 SessionProvider | internal/base 由测试决定 | 测试 route |
| `novel_task`、`novel_project` | 当前产品明确停用 | 不发布 | 无 |

### Task 1: 建立跨平台 `AiBrainPaths`

**Files:**
- Create: `rust/crates/brain-core/src/paths.rs`
- Modify: `rust/crates/brain-core/src/lib.rs`
- Modify: `rust/crates/brain-core/src/config.rs`

- [ ] **Step 1: 写路径优先级和失败语义 RED 测试**

在 `paths.rs` 的测试模块写纯函数测试，不修改进程环境：

```rust
#[test]
fn explicit_ai_brain_home_has_highest_priority() {
    let paths = AiBrainPaths::resolve_with(
        PathBuf::from("D:/work/repo"),
        |key| match key {
            "AI_BRAIN_HOME" => Some(OsString::from("D:/portable/brain")),
            "HOME" => Some(OsString::from("D:/wrong-home")),
            _ => None,
        },
    ).unwrap();

    assert_eq!(paths.user_root(), Path::new("D:/portable/brain"));
    assert_eq!(paths.project_root(), Path::new("D:/work/repo/.ai-brain"));
}

#[test]
fn userprofile_is_used_when_home_is_absent() {
    let paths = AiBrainPaths::resolve_with(
        PathBuf::from("D:/work/repo"),
        |key| (key == "USERPROFILE").then(|| OsString::from("C:/Users/tester")),
    ).unwrap();
    assert_eq!(paths.user_root(), Path::new("C:/Users/tester/.ai-brain"));
}

#[test]
fn missing_home_is_an_error_and_never_tmp() {
    let error = AiBrainPaths::resolve_with(PathBuf::from("D:/work/repo"), |_| None).unwrap_err();
    assert!(matches!(error, AiBrainPathError::HomeUnavailable));
    assert!(!error.to_string().contains("/tmp"));
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-core paths --lib -- --nocapture`

Expected: 编译失败，因为 `AiBrainPaths` 尚不存在。

- [ ] **Step 3: 实现唯一解析器**

生产类型必须采用以下公开面：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiBrainPaths {
    user_root: PathBuf,
    project_root: PathBuf,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AiBrainPathError {
    #[error("无法确定用户目录；请设置 AI_BRAIN_HOME、HOME 或 USERPROFILE")]
    HomeUnavailable,
    #[error("工作区路径为空")]
    WorkspaceUnavailable,
}

impl AiBrainPaths {
    pub fn resolve(workspace_root: impl Into<PathBuf>) -> Result<Self, AiBrainPathError> {
        Self::resolve_with(workspace_root, std::env::var_os)
    }

    pub fn resolve_with<F>(
        workspace_root: impl Into<PathBuf>,
        get_env: F,
    ) -> Result<Self, AiBrainPathError>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        let workspace_root = workspace_root.into();
        if workspace_root.as_os_str().is_empty() {
            return Err(AiBrainPathError::WorkspaceUnavailable);
        }
        let workspace_root = absolute_clean_path(workspace_root)
            .map_err(|_| AiBrainPathError::WorkspaceUnavailable)?;
        let user_root = get_env("AI_BRAIN_HOME").filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| get_env("HOME").filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".ai-brain")))
            .or_else(|| get_env("USERPROFILE").filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".ai-brain")))
            .ok_or(AiBrainPathError::HomeUnavailable)?;
        let user_root = absolute_clean_path(user_root)
            .map_err(|_| AiBrainPathError::HomeUnavailable)?;
        Ok(Self {
            user_root,
            project_root: workspace_root.join(".ai-brain"),
        })
    }

    pub fn user_root(&self) -> &Path { &self.user_root }
    pub fn project_root(&self) -> &Path { &self.project_root }
    pub fn config_file(&self) -> PathBuf { self.user_root.join("config.toml") }
    pub fn memory_root(&self) -> PathBuf { self.user_root.join("memory") }
    pub fn graph_db(&self) -> PathBuf { self.user_root.join("graph").join("graph.db") }
    pub fn mcp_config(&self) -> PathBuf { self.user_root.join("mcp").join("mcp-servers.json") }
    pub fn project_mcp_config(&self) -> PathBuf {
        self.project_root.join("mcp").join("mcp-servers.json")
    }
    pub fn plugins_root(&self) -> PathBuf { self.user_root.join("plugins") }
    pub fn skills_root(&self) -> PathBuf { self.user_root.join("skills") }
    pub fn project_skills_root(&self) -> PathBuf { self.project_root.join("skills") }
    pub fn evolution_root(&self) -> PathBuf { self.user_root.join("evolution") }
    pub fn credentials_root(&self) -> PathBuf { self.user_root.join("credentials") }
}
```

`absolute_clean_path` 对相对 `AI_BRAIN_HOME` 直接报错，对尚未创建的绝对 home 只做 lexical normalize（不要求目录已存在）；后续写入方安全创建具体子目录。导出 `pub mod paths`。给 `BrainConfig` 增加 `from_paths(&AiBrainPaths)` 和 `try_default()`；生产调用改用返回 `Result` 的接口。保留 `Default` 只为现有测试/兼容调用，内部调用 `try_default().expect("无法解析智脑目录，请设置 AI_BRAIN_HOME、HOME 或 USERPROFILE")`，禁止任何 `PathBuf::from("/tmp")` 或 `PathBuf::from(".")` 回退。

- [ ] **Step 4: 运行 GREEN 并提交**

Run:

```powershell
cargo test -p brain-core paths --lib -- --nocapture
cargo test -p brain-core config --lib -- --nocapture
cargo clippy -p brain-core --all-targets -- -D warnings
```

Expected: 路径测试通过，错误文案明确，Clippy 退出 0。

```powershell
git add crates/brain-core/src/paths.rs crates/brain-core/src/lib.rs crates/brain-core/src/config.rs
git commit -m "feat(paths): centralize ai brain directories"
```

### Task 2: 探测并注入真实 shell backend

**Files:**
- Modify: `rust/crates/runtime/src/bash.rs`
- Modify: `rust/crates/runtime/src/lib.rs`
- Modify: `rust/crates/tools/src/lib.rs`

- [ ] **Step 1: 写无 `sh`、有 PowerShell 和显式 launcher RED 测试**

用临时目录创建空文件名模拟 PATH，测试 finder 本身而不调用系统 shell：

```rust
#[test]
fn windows_path_without_sh_never_reports_bash() {
    let availability = ShellAvailability::detect_in(
        &[PathBuf::from("C:/empty")],
        &[".EXE".into()],
        ShellPlatform::Windows,
        |path| path == Path::new("C:/empty/pwsh.EXE"),
    );
    assert!(availability.sh().is_none());
    assert_eq!(availability.powershell(), Some(Path::new("C:/empty/pwsh.EXE")));
}

#[tokio::test]
async fn injected_launcher_is_used_for_execution() {
    let launcher = ShellBackend::Sh(fixture_shell_path());
    let output = execute_shell_in_dir(
        BashCommandInput {
            command: "printf injected".into(),
            timeout: None,
            description: None,
            run_in_background: Some(false),
            dangerously_disable_sandbox: None,
            namespace_restrictions: None,
            isolate_network: None,
            filesystem_mode: None,
            allowed_mounts: None,
        },
        temp_workspace().path(),
        &launcher,
    ).unwrap();
    assert!(output.stdout.contains("injected"));
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p runtime shell_backend --lib -- --nocapture`

Expected: 编译失败，新的探测和执行 API 尚未定义。

- [ ] **Step 3: 实现探测、执行和兼容 wrapper**

增加：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellBackend {
    Sh(PathBuf),
    PowerShell(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPlatform { Windows, Unix }

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellAvailability {
    sh: Option<PathBuf>,
    powershell: Option<PathBuf>,
}

impl ShellAvailability {
    pub fn detect() -> Self;
    #[cfg(test)]
    pub(crate) fn detect_in<F>(
        search_paths: &[PathBuf],
        path_extensions: &[OsString],
        platform: ShellPlatform,
        exists: F,
    ) -> Self
    where
        F: Fn(&Path) -> bool;
    pub fn sh(&self) -> Option<&Path>;
    pub fn powershell(&self) -> Option<&Path>;
}
```

实现约束：

- 使用 `std::env::split_paths(PATH)` 和 Windows `PATHEXT` 枚举候选，不通过另一个 shell 执行 `where`/`which`。
- Windows PowerShell 优先级固定为 `pwsh`、`powershell.exe`；sh 候选固定为 `sh.exe`、`sh`。
- Unix 候选固定为 `sh`，并检查 executable permission；测试注入文件存在判断。
- `execute_shell_in_dir(input, cwd, backend)` 根据 backend 构造命令；Sh 使用 `-c`，PowerShell 使用 `-NoLogo -NoProfile -NonInteractive -Command`。
- 旧 `execute_bash[_in_dir]` 仅调用 `ShellAvailability::detect().sh()`；缺少时返回 `io::ErrorKind::NotFound`，不再无条件 `Command::new("sh")`。
- tools 的 `PowerShell` 路径接受注入 backend，不再自行猜命令名。

- [ ] **Step 4: GREEN、平台回归和提交**

Run:

```powershell
cargo test -p runtime shell_backend --lib -- --nocapture
cargo test -p runtime bash --lib -- --nocapture
cargo test -p tools powershell --lib -- --nocapture
cargo clippy -p runtime -p tools --all-targets -- -D warnings
```

Expected: fixture 执行通过；缺少 sh 时返回 NotFound；现有 sandbox/timeout 测试保持通过。

```powershell
git add crates/runtime/src/bash.rs crates/runtime/src/lib.rs crates/tools/src/lib.rs
git commit -m "fix(runtime): inject detected shell backends"
```

### Task 3: 实现按真实依赖发布的 `BuiltinProvider`

**Files:**
- Create: `rust/crates/tools/src/provider.rs`
- Modify: `rust/crates/tools/src/lib.rs`
- Modify: `rust/crates/tools/Cargo.toml`

- [ ] **Step 1: 写能力矩阵 RED 测试**

测试至少覆盖：

```rust
#[tokio::test]
async fn provider_omits_capabilities_without_backends() {
    let provider = BuiltinProvider::new(
        recording_executor(),
        BuiltinCapabilities::minimal(temp_workspace()),
    );
    let contribution = provider.discover().await.unwrap();
    let names = names(&contribution);
    assert!(names.contains("read_file"));
    assert!(names.contains("ToolSearch"));
    for absent in ["bash", "PowerShell", "Skill", "Agent", "search_memory",
        "graph_search_catalog", "WebSearch", "LSP", "RemoteTrigger", "MCP",
        "TestingPermission", "novel_task"] {
        assert!(!names.contains(absent), "不应发布 {absent}");
    }
}

#[tokio::test]
async fn every_published_builtin_has_the_expected_route_kind() {
    let contribution = fully_available_fixture_provider().discover().await.unwrap();
    for item in contribution.registrations {
        match item.canonical_name.as_str() {
            "ToolSearch" => assert_eq!(item.route.kind(), ToolRouteKind::ToolSearch),
            "AskUserQuestion" => assert_eq!(item.route.kind(), ToolRouteKind::AskUser),
            _ => assert_eq!(item.route.kind(), ToolRouteKind::Backend),
        }
    }
}
```

再写 Schema 兼容测试，逐项比较 `read_file`、`WebSearch`（尚不发布但 catalog 仍保留）、`Skill`、`Agent` 的 descriptor 与当前 `mvp_tool_specs()` 对应字段完全相等。

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p tools builtin_provider --lib -- --nocapture`

Expected: 编译失败，因为 provider/capabilities 尚不存在。

- [ ] **Step 3: 添加能力对象与确定性注册构建**

公开接口：

```rust
#[derive(Clone)]
pub struct BuiltinCapabilities {
    pub workspace_root: PathBuf,
    pub workspace_write: bool,
    pub shell: ShellAvailability,
    pub ask_user: bool,
    pub todo: bool,
    pub notebook: bool,
    pub sleep: bool,
    pub messaging: bool,
    pub configuration: bool,
    pub plan_mode: bool,
    pub repl: bool,
    pub structured_output: bool,
    pub agent_runtime: bool,
    pub skills: bool,
    pub memory: bool,
    pub graph: bool,
}

pub struct BuiltinProvider {
    executor: Arc<dyn ToolExecutor>,
    capabilities: BuiltinCapabilities,
}
```

实现要求：

- provider id 固定为 `builtin`，`source_instance` 写规范化 workspace。
- descriptor 从既有 ToolSpec catalog 逐名取得，禁止复制 Schema。
- `ToolPermission`、risk、side_effecting、network_access、requires_confirmation 由一张仅描述安全属性的映射产生；未知 ToolSpec 返回错误，不能默认放行。
- base/deferred 严格按本计划能力矩阵。
- 标准工具使用 `ExecutorToolRoute`；`ToolSearch`/`AskUserQuestion` 使用 `SystemToolRoute`。
- bash route 包装后把已探测 `ShellBackend::Sh` 传到底层；PowerShell 同理。
- `read_file/glob_search/grep_search/ToolSearch/Sleep/StructuredOutput` 的 flag 只在对应真实函数可调用时为 true；Todo、Notebook、Config、计划模式、REPL 还必须有已规范化 workspace，SendUserMessage 必须有真实消息 sink，不能因为 ToolSpec 存在就设 true。
- 注册安全属性显式设置 guard profile：bash/PowerShell/REPL 为 `ShellCommand`，write/edit/notebook 为 `SensitivePath`；只有工具输入 Schema 本身含目标 URL 的 WebFetch/同类工具才使用 `RemoteUrl`。Agent 及其他外部调用通过 permission/risk/network_access/confirmation 控制，连接 endpoint 由各 Provider 在配置、DNS 与 redirect 层校验，不能把任意业务参数误当 URL。alias 与 canonical 共用这份 metadata。
- 注册结果按 canonical name 排序；capability false 时完全没有 registration。
- contribution 使用 `.with_generation(...)` 写入由 capability flags、workspace identity、shell backend 与各真实 dependency revision 组成的稳定配置指纹；同样的探测结果不制造新版本，执行 handle/依赖代次改变则必须替换 route。
- `mvp_tool_specs()` 暂时保留为 Schema 兼容入口，但添加 `#[deprecated(note = "生产 v2 使用 BuiltinProvider")]`；生产代码从本任务结束起不得调用它来决定可用性。

- [ ] **Step 4: GREEN、执行一致性和提交**

Run:

```powershell
cargo test -p tools builtin_provider --lib -- --nocapture
cargo test -p tools tool_spec --lib -- --nocapture
cargo clippy -p tools --all-targets -- -D warnings
```

Expected: 能力矩阵、route kind、Schema 兼容测试通过。

```powershell
git add crates/tools/Cargo.toml crates/tools/src/lib.rs crates/tools/src/provider.rs
git commit -m "feat(tools): publish truthful builtin capabilities"
```

### Task 4: 让 `RealToolExecutor` 与首版 `ToolRuntime` 同源组装 builtin

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/real_tool_executor.rs`
- Create: `rust/crates/ai-brain-cli/src/tool_runtime.rs`
- Modify: `rust/crates/ai-brain-cli/src/lib.rs`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

- [ ] **Step 1: 写生产组装不使用静态全集的 RED 测试**

在 `tool_runtime.rs` 写：

```rust
#[tokio::test]
async fn initial_refresh_publishes_exact_executor_capabilities() {
    let registry = Arc::new(DynamicToolRegistry::new());
    let runtime = ToolRuntime::new_for_test(Arc::clone(&registry), fixture_paths());
    runtime.refresh_builtin(fixture_builtin_dependencies()).await.unwrap();
    let snapshot = registry.snapshot();
    assert!(snapshot.resolve("read_file").is_some());
    assert!(snapshot.resolve("search_memory").is_none());
    assert!(snapshot.registrations().iter().all(|item| item.provider_id == "builtin"));
}
```

在 `orchestrator.rs` 增加一个构造测试，断言 MainBrain 和 Motor 持有的 registry version 与 `ToolRuntime::registry().snapshot().version()` 相同；不要通过源代码字符串测试行为。

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p ai-brain-cli tool_runtime --lib -- --nocapture`

Expected: 编译失败，`ToolRuntime` 尚不存在。

- [ ] **Step 3: 收紧 RealToolExecutor 的职责**

修改规则：

- 删除构造时 `mvp_tool_specs()` → `tool_descriptors` 的全集缓存。
- `list_tools()` 仅作为旧 `ToolExecutor` adapter，返回构造时由 `BuiltinCapabilities` 明确传入的 descriptor；动态 MainBrain 不调用它。
- 增加不可变 `RealToolDependencies`，集中保存 `memory_brain`、`skill_catalog_snapshot`、`dispatch`、`graph_db_path`、`shell`、`message_sink`、`runtime_trace_tx` 和 `novel_application` 的现有强类型 handle；由 `try_new(paths, handles)` 验证 workspace/graph/shell 后构造，`builtin_capabilities(workspace, ask_user)` 只能从这些 `Option`/探测结果派生 flag。后续子计划 C 将 skill/MCP/plugin 从此兼容 executor 拆到各自 route。
- `execute_internal` 的未知工具路径仍返回 `is_error=true`；不增加任何“成功但未实现”的分支。
- Skill/memory/graph/Agent 分支继续使用真实 handle，provider 只有在相应 handle 存在时才发布。
- 现有 `novel_task/novel_project` 即使 executor handle 存在也不进入本次 BuiltinProvider（产品矩阵明确停用），避免迁移时意外恢复。

- [ ] **Step 4: 实现 builtin-only `ToolRuntime`**

本阶段公开接口固定为：

```rust
pub struct ToolRuntime {
    registry: Arc<DynamicToolRegistry>,
    paths: AiBrainPaths,
    builtin: tokio::sync::RwLock<Option<Arc<BuiltinProvider>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolRuntimeError {
    #[error("工具 Provider {provider_id} 刷新失败: {message}")]
    Provider { provider_id: String, message: String },
    #[error("工具注册表更新失败: {0}")]
    Registry(String),
    #[error("工具运行时关闭超时")]
    ShutdownTimeout,
}

impl ToolRuntime {
    pub fn new(registry: Arc<DynamicToolRegistry>, paths: AiBrainPaths) -> Self;
    pub fn registry(&self) -> Arc<DynamicToolRegistry>;
    pub fn paths(&self) -> &AiBrainPaths;
    pub async fn refresh_builtin(
        &self,
        provider: Arc<BuiltinProvider>,
    ) -> Result<Arc<ToolSnapshot>, ToolRuntimeError>;
}
```

`refresh_builtin` 必须先在 registry 锁外 `provider.discover().await`，再调用 `replace_provider`；失败时调用 `mark_provider_error("builtin", ..., true)`，不能清空上一健康 contribution。

- [ ] **Step 5: 改变 Orchestrator 初始化顺序**

按以下顺序执行：

1. 在获得 workspace 和 `AiBrainPaths` 后立即创建空 `Arc<DynamicToolRegistry>`。
2. 用该 view 创建 `MotorBrain`；此时空快照合法。
3. 初始化 memory、graph、dispatch、skills 等 handle。
4. 构造 `RealToolDependencies`、`RealToolExecutor` 和 `BuiltinProvider`。
5. 调用 `ToolRuntime::refresh_builtin`。
6. `MainBrain::new_with_registry_in_context` 接受同一个 registry。
7. 删除生产 `mvp_tool_definitions()`、`brain.register_tools(tool_defs)` 和技能摘要手工注入；摘要来自 provider snapshot 的 prompt fragment。

LLM 创建失败时保留 ToolRuntime 的真实状态，只把 `v2_brain` 设为 None。

- [ ] **Step 6: GREEN、静态引用检查和提交**

Run:

```powershell
cargo test -p ai-brain-cli tool_runtime --lib -- --nocapture
cargo test -p ai-brain-cli orchestrator --lib -- --nocapture
rg -n "mvp_tool_definitions\(|register_tools\(tool_defs\)|inject_skill_summary" crates/ai-brain-cli/src/orchestrator.rs
cargo clippy -p ai-brain-cli --all-targets -- -D warnings
```

Expected: 测试通过；`rg` 无输出；Clippy 退出 0。

```powershell
git add crates/ai-brain-cli/src/lib.rs crates/ai-brain-cli/src/orchestrator.rs crates/ai-brain-cli/src/real_tool_executor.rs crates/ai-brain-cli/src/tool_runtime.rs
git commit -m "feat(cli): assemble v2 tools through runtime registry"
```

### Task 5: Motor Brain 改为动态快照消费者和真实 route 执行者

**Files:**
- Modify: `rust/crates/brain-motor/src/motor_brain.rs`
- Modify: `rust/crates/brain-motor/src/error.rs`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

- [ ] **Step 1: 用动态合同替换旧内置/桩测试**

删除 `builtin_tools_registered`、`Read/Edit/Bash` 固定名称断言和“执行成功但输出含 stub”的测试，替换为：

```rust
#[test]
fn fast_think_suggests_only_tools_present_in_snapshot_metadata() {
    let registry = registry_with(vec![registration_with_scenarios(
        "workspace_reader", &["文件", "读取"], ToolRiskLevel::Low,
    )]);
    let brain = MotorBrain::new(MotorConfig::default(), registry).unwrap();
    let result = brain.fast_think(&make_broadcast("请读取这个文件"));
    assert_eq!(result.suggested_tools, vec!["workspace_reader"]);
}

#[tokio::test]
async fn execute_tool_calls_route_from_current_snapshot() {
    let route = Arc::new(RecordingRoute::success("真实结果"));
    let registry = registry_with(vec![registration_with_route("dynamic_read", route.clone())]);
    let brain = MotorBrain::new(MotorConfig::default(), registry).unwrap();
    let result = brain.execute_tool(&call("dynamic_read", true), &context()).await.unwrap();
    assert_eq!(result.output, "真实结果");
    assert_eq!(route.calls(), 1);
}

#[tokio::test]
async fn unknown_tool_is_rejected_before_any_route() {
    let brain = MotorBrain::new(MotorConfig::default(), empty_registry()).unwrap();
    assert!(matches!(
        brain.execute_tool(&call("Read", true), &context()).await,
        Err(MotorError::ToolNotRegistered(name)) if name == "Read"
    ));
}

#[tokio::test]
async fn model_validated_flag_cannot_bypass_motor_confirmation() {
    let route = Arc::new(RecordingRoute::success("不应执行"));
    let registry = registry_with(vec![dangerous_registration("dynamic_write", route.clone())]);
    let brain = MotorBrain::new(MotorConfig::default(), registry).unwrap();
    assert!(matches!(
        brain.execute_tool(&call("dynamic_write", true), &context()).await,
        Err(MotorError::ValidationFailed(_))
    ));
    assert_eq!(route.calls(), 0);
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-motor motor_brain --lib -- --nocapture`

Expected: 旧 constructor/signature/registry 使测试编译失败。

- [ ] **Step 3: 实现动态 Motor API**

`MotorBrain` 字段改为 `registry: Arc<dyn ToolRegistryView>`；构造器固定为 `new(config, registry)`。实现：

- `list_tools()` 每次取最新 snapshot，只投影非 internal registration。
- `fast_think()` 将消息小写分词，与 registration 的 `search_terms`、`scenarios` 及 description 匹配；不再包含关键词→工具名数组。
- `slow_think()` 在方法开始取一个 snapshot，用该快照生成 prompt；LLM 回复不得引用不存在的名称。
- `execute_tool(call, context)` 为 async：取一次 snapshot、resolve、调用 `brain_core::tool_policy::ToolAuthorizationPolicy`，再执行 registration.route；不得让 `brain-motor` 依赖 `brain-main`。
- Motor 没有宿主确认通道，调用策略时固定传 `ToolApproval::None`；任何 `RequireConfirmation` 映射 `ValidationFailed`，不得读取或信任模型可写的 `call.validated/validation_id`。未知工具始终 `ToolNotRegistered`。
- `tool_risk_level(name)` 返回 `Option<ToolRiskLevel>`，未知为 None；权限代码不可把 None 当低风险。
- 删除旧 `ToolCapability`、`with_builtin_tools()`、`execute_stub()` 和旧 tool registry 搜索表。

- [ ] **Step 4: 更新 Orchestrator 并运行 GREEN**

Run:

```powershell
cargo test -p brain-motor --lib -- --nocapture
cargo test -p ai-brain-cli motor --lib -- --nocapture
cargo clippy -p brain-motor -p ai-brain-cli --all-targets -- -D warnings
```

Expected: Motor 测试使用任意动态名称通过，源码不存在 `execute_stub`/`with_builtin_tools`。

```powershell
git add crates/brain-motor/src/motor_brain.rs crates/brain-motor/src/error.rs crates/brain-motor/src/tool_registry.rs crates/ai-brain-cli/src/orchestrator.rs
git commit -m "refactor(motor): consume dynamic tool snapshots"
```

### Task 6: Eval Brain 从快照派生只读视图

**Files:**
- Modify: `rust/crates/brain-eval/src/eval_brain.rs`
- Modify: `rust/crates/brain-eval/Cargo.toml`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

- [ ] **Step 1: 写 Schema 同源和策略 RED 测试**

```rust
#[test]
fn eval_view_uses_registered_schema_and_excludes_writes() {
    let snapshot = snapshot_with(vec![
        registration("read_file", ToolPermission::ReadOnly, json!({"required":["path"]})),
        registration("edit_file", ToolPermission::WorkspaceWrite, json!({"required":["path"]})),
        registration("Skill", ToolPermission::ReadOnly, json!({"required":["skill"]})),
    ]);
    let view = EvalToolView::from_snapshot(snapshot);
    assert_eq!(view.descriptor("read_file").unwrap().input_schema["required"], json!(["path"]));
    assert_eq!(view.descriptor("Skill").unwrap().input_schema["required"], json!(["skill"]));
    assert!(view.descriptor("edit_file").is_none());
}

#[tokio::test]
async fn eval_bash_keeps_parameter_level_read_only_guard() {
    let route = Arc::new(RecordingRoute::success("ok"));
    let view = eval_view_with_bash(route.clone());
    let error = execute_eval_call(&view, call("bash", json!({"command":"rm file"})), &context())
        .await.unwrap_err();
    assert!(error.to_string().contains("只读"));
    assert_eq!(route.calls(), 0);
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p brain-eval eval_view --lib -- --nocapture`

Expected: 编译失败，EvalToolView 尚不存在。

- [ ] **Step 3: 实现派生视图和 route loop**

- 删除 `build_read_only_tool_definitions()` 的五个手写 schema。
- `EvalToolView::from_snapshot` 选择 `ToolPermission::ReadOnly` 且 exposure 非 Internal 的原 registration；bash 是唯一衰减能力例外：从原 descriptor/route 构造只存在于 `EvalToolView` 的 `EvalReadOnlyShellRegistration`，metadata 降为 ReadOnly，但 route 必须包一层 `EvalReadOnlyShellRoute`，先用严格 parser/allowlist 校验再委托固定的原 route。这样共享 `ToolAuthorizationPolicy::read_only()` 可以验证衰减后的 registration，同时危险原 route 不能绕过 wrapper。
- `is_read_only_bash_command` 只允许单条或由 `&&` 连接的 `pwd/ls/find/rg/grep/cat/head/tail/wc/git status/git diff/git log/cargo check/cargo test` 及只读参数；显式拒绝 `rm`、重定向、管道、`;`、`||`、subshell、变量展开、命令替换、绝对写路径和未知命令。禁止靠通用危险字符串表证明“只读”。
- descriptor 直接 clone registration.descriptor，禁止改名字段。
- eval tool loop 接受 `ToolRequestView`/`EvalToolView` 与 `ToolExecutionContext`，在广告校验后执行 snapshot route；删除独立 `ToolExecutor` 二次查表。
- `EvalBrain::with_verification(llm, registry, context)` 接受 registry view；每次 evaluate 开头固定 snapshot。
- Skill 是否出现及其 input key 完全由 Catalog 对应 registration 决定；保留 `SkillRegistry` 作为内置审查内容数据源，但不再另造 ToolDefinition。
- Eval 没有交互确认通道；任何 `RequireConfirmation` 一律映射成明确 PermissionDenied，绝不能自行把 call 标记为 validated。
- 测试除 `rm` 外再覆盖 `cat x | tee y`、`echo x > y`、`$(touch y)` 和未知命令均在 inner route 前拒绝，`git status && cargo check` 可执行且仍只调用原 route 一次。

- [ ] **Step 4: GREEN、兼容测试和提交**

Run:

```powershell
cargo test -p brain-eval --lib -- --nocapture
cargo test -p ai-brain-cli eval --lib -- --nocapture
cargo clippy -p brain-eval -p ai-brain-cli --all-targets -- -D warnings
```

Expected: 原评估循环/输出截断测试通过，Schema 断言改为与 registry 同源。

```powershell
git add crates/brain-eval/Cargo.toml crates/brain-eval/src/eval_brain.rs crates/ai-brain-cli/src/orchestrator.rs
git commit -m "refactor(eval): derive tools from registry snapshots"
```

### Task 7: 协作工具改为真正的 SessionProvider

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration_tools.rs`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`
- Modify: `rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs`

- [ ] **Step 1: 写会话隔离 RED 测试**

```rust
#[tokio::test]
async fn group_tool_is_added_only_to_the_derived_member_view() {
    let base = registry_with_read_file();
    let session = group_message_contribution(scope_fixture());
    let derived = base.snapshot().derive_session(session).unwrap();
    assert!(base.snapshot().resolve(READ_GROUP_MESSAGES_TOOL).is_none());
    assert!(derived.resolve(READ_GROUP_MESSAGES_TOOL).is_some());
    assert!(derived.resolve("read_file").is_some());
}

#[tokio::test]
async fn allow_tools_false_has_an_empty_advertisement_without_global_mutation() {
    let runtime = member_runtime_fixture();
    let output = runtime.run_member(false).await.unwrap();
    assert!(output.observed_tool_names.is_empty());
    assert!(runtime.registry().snapshot().resolve("read_file").is_some());
}
```

- [ ] **Step 2: 运行 RED**

Run: `cargo test -p ai-brain-cli group_message_tool --lib -- --nocapture`

Expected: 旧 `GroupMessageToolExecutor + Vec<ToolDefinition>` 路径无法满足测试。

- [ ] **Step 3: 实现 session contribution**

- 将 `group_message_tool_definition()` 替换为 `group_message_tool_registration(scope)`。
- route 只处理 `read_group_messages`，直接持有 `GroupMessageToolScope`；普通 builtin 不再通过包装 executor 转发。
- `group_message_contribution` 的 provider id 为 `session:group:<room-id>:<claim-sequence>`，registration provider id 必须一致，并以 scope/claim sequence 作为非空 generation。
- Orchestrator 的成员 fork 传入 `Option<ProviderContribution>`；MainBrain 在请求入口 derive，global registry 不改变。
- `allow_tools=false` 使用 `ToolRequestView::empty(derived_or_base_snapshot)`，不能调用 `register_tools(Vec::new())`。
- collaboration runtime fixture 改用 `StaticToolRegistry` + 明确 registrations；删除 `mvp_tool_definitions()` import。

- [ ] **Step 4: GREEN 和提交**

Run:

```powershell
cargo test -p ai-brain-cli group_message_tool --lib -- --nocapture
cargo test -p ai-brain-cli collaboration_runtime --lib -- --nocapture
cargo test -p ai-brain-cli --test v2_integration_test -- --nocapture
```

Expected: 会话边界、工作目录、claim sequence 和 allow_tools 测试全部通过。

```powershell
git add crates/ai-brain-cli/src/orchestrator.rs crates/ai-brain-cli/src/web/collaboration_tools.rs crates/ai-brain-cli/src/web/collaboration_runtime.rs
git commit -m "refactor(collaboration): inject session tool providers"
```

### Task 8: 迁移路径消费者并建立阶段集成门禁

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/config_manager.rs`
- Modify: `rust/crates/ai-brain-cli/src/init.rs`
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`
- Modify: `rust/crates/brain-integration-tests/Cargo.toml`
- Create: `rust/crates/brain-integration-tests/tests/dynamic_tools.rs`

- [ ] **Step 1: 写 Windows 路径与动态生产组装 RED 集成测试**

`dynamic_tools.rs` 至少包含：

```rust
#[tokio::test]
async fn v2_builtin_snapshot_contains_no_unavailable_or_fake_capability() {
    let fixture = DynamicRuntimeFixture::without_external_backends().await;
    let names = fixture.snapshot_names();
    assert!(names.contains("read_file"));
    for absent in ["LSP", "RemoteTrigger", "MCP", "TestingPermission",
        "ListMcpResources", "ReadMcpResource", "WebSearch"] {
        assert!(!names.contains(absent));
    }
    fixture.assert_every_advertised_route_executes_or_returns_typed_backend_error().await;
}

#[test]
fn userprofile_resolves_every_runtime_consumer_to_one_root() {
    let fixture = WindowsPathFixture::new("C:/Users/brain-user");
    assert_eq!(fixture.config_path().parent(), fixture.mcp_path().ancestors().nth(2));
    assert!(fixture.all_paths().iter().all(|path| path.starts_with("C:/Users/brain-user/.ai-brain")));
}
```

- [ ] **Step 2: 迁移所有本阶段路径消费者**

- 在 `brain-integration-tests/Cargo.toml` 增加测试 fixture 直接使用的 `ai-brain-cli`、`brain-main`、`brain-eval`、`runtime` 路径依赖；若 fixture helper 最终下沉到 `tools`，只保留实际被 test import 的直接依赖，不能依赖传递依赖碰巧可见。
- `ConfigManager::try_new(&AiBrainPaths)` 使用 `paths.config_file()`；保留 `new()` 只做显式 expect 兼容。
- `init::base_dir()` 改为 `base_dir(paths: &AiBrainPaths) -> &Path` 或删除，由调用方持有 paths。
- Orchestrator 的 memory、graph、MCP、plugin、skill、evolution 路径全部从同一 `AiBrainPaths` 获得。
- 本阶段仅修改智脑目录；`runtime` 自有 `.claw` 配置仍保持其产品语义，MCP canonical config 在子计划 C 收敛。
- 用 `rg` 检查 v2 生产路径没有 HOME-only 或 `/tmp` 回退。

- [ ] **Step 3: 运行完整阶段门禁**

Run:

```powershell
cargo test -p brain-integration-tests --test dynamic_tools -- --nocapture
cargo test -p tools -p brain-motor -p brain-eval -p brain-main
cargo test -p ai-brain-cli collaboration --lib -- --nocapture
cargo fmt --check
cargo clippy -p brain-core -p runtime -p tools -p brain-motor -p brain-eval -p ai-brain-cli --all-targets -- -D warnings
rg -n 'unwrap_or_else\(\|_\| ("/tmp"|"\."\.into\(\))|std::env::var\("HOME"\)' crates/brain-core/src crates/ai-brain-cli/src/orchestrator.rs crates/ai-brain-cli/src/config_manager.rs crates/ai-brain-cli/src/init.rs
```

Expected: 测试/格式/Clippy 通过；最后 `rg` 无 v2 路径回退命中。若 regex 命中测试 fixture，只把搜索范围缩到生产函数并记录原因，不删除有效测试。

- [ ] **Step 4: 提交阶段结果**

```powershell
git add crates/ai-brain-cli/src/config_manager.rs crates/ai-brain-cli/src/init.rs crates/ai-brain-cli/src/orchestrator.rs crates/brain-integration-tests/Cargo.toml crates/brain-integration-tests/tests/dynamic_tools.rs
git diff --cached --check
git commit -m "test(v2): lock dynamic builtin integration"
```

阶段完成条件：v2 生产入口只从 registry 获取 builtin；Motor/Eval/协作不再拥有静态工具真相源；Windows USERPROFILE 和无 shell 场景均有自动化证据。
