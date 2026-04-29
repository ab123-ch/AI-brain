# Hook 系统实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 新建 brain-hooks crate，实现可扩展的 hook 框架，让主脑自决策是否触发评估脑，节省 token。

**Architecture:** 新建 `brain-hooks` crate（5 个文件），参考 runtime/hooks.rs 的 command 执行模式，扩展 Brain 层事件类型（PostQuery/OnShutdown/SessionStart）。通过 config.toml `[hooks]` 段配置，orchestrator 和 tool_loop 调用 HookRunner。

**Tech Stack:** Rust, serde/toml 配置, tokio 异步, brain-llm（eval_gate 用）

---

### Task 1: 创建 brain-hooks crate 骨架

**Files:**
- Create: `rust/crates/brain-hooks/Cargo.toml`
- Create: `rust/crates/brain-hooks/src/lib.rs`
- Create: `rust/crates/brain-hooks/src/types.rs`

**Step 1: 创建 Cargo.toml**

```toml
[package]
name = "brain-hooks"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tracing = "0.1"
tokio = { version = "1", features = ["rt", "time", "process", "sync", "macros"] }
brain-llm = { path = "../brain-llm" }

[lints]
workspace = true
```

**Step 2: 创建 lib.rs**

```rust
pub mod types;
pub mod config;
pub mod runner;
pub mod builtins;
```

**Step 3: 创建 types.rs — 核心类型定义**

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Hook 事件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HookEvent {
    /// 工具执行前（tool_loop 内）
    PreToolUse,
    /// 工具执行后（tool_loop 内）
    PostToolUse,
    /// 主脑回复完成后（orchestrator 内）
    PostQuery,
    /// 会话关闭时
    OnShutdown,
    /// 会话启动时
    SessionStart,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostQuery => "PostQuery",
            Self::OnShutdown => "OnShutdown",
            Self::SessionStart => "SessionStart",
        }
    }
}

/// 工具名匹配器
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    /// 匹配所有（matcher 为空、"*"、或省略）
    MatchAll,
    /// 管道分隔的多工具名匹配（"Bash|Edit|Write"）
    PipeDelimited(Vec<String>),
}

impl Matcher {
    /// 从配置字符串解析匹配器
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            None | Some("") | Some("*") => Self::MatchAll,
            Some(s) => Self::PipeDelimited(s.split('|').map(String::from).collect()),
        }
    }

    /// 检查工具名是否匹配
    pub fn matches(&self, tool_name: &str) -> bool {
        match self {
            Self::MatchAll => true,
            Self::PipeDelimited(names) => names.iter().any(|n| n == tool_name),
        }
    }
}

/// Hook handler 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HookHandlerConfig {
    /// Shell 命令执行
    #[serde(rename = "command")]
    Command {
        command: String,
        #[serde(default)]
        matcher: Option<String>,
        #[serde(default = "default_timeout")]
        timeout: u64,
    },
    /// 内置 handler
    #[serde(rename = "builtin")]
    Builtin {
        name: String,
    },
}

fn default_timeout() -> u64 {
    30
}

/// Hook 输入上下文
#[derive(Debug, Clone)]
pub struct HookInput {
    pub event: HookEvent,
    pub session_id: String,
    pub cwd: PathBuf,
    // 工具层字段（PreToolUse / PostToolUse）
    pub tool_name: Option<String>,
    pub tool_input: Option<String>,
    pub tool_output: Option<String>,
    pub is_error: bool,
    // Brain 层字段（PostQuery）
    pub user_input: Option<String>,
    pub ai_output: Option<String>,
}

/// Hook 决策
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    Deny,
}

/// Hook 执行输出
#[derive(Debug, Clone)]
pub struct HookOutput {
    pub decision: HookDecision,
    pub reason: Option<String>,
    /// PostQuery 专用：是否触发评估脑
    pub trigger_eval: bool,
    /// 注入到后续流程的系统消息
    pub system_message: Option<String>,
}

impl HookOutput {
    pub fn allow() -> Self {
        Self {
            decision: HookDecision::Allow,
            reason: None,
            trigger_eval: false,
            system_message: None,
        }
    }

    pub fn deny(reason: String) -> Self {
        Self {
            decision: HookDecision::Deny,
            reason: Some(reason),
            trigger_eval: false,
            system_message: None,
        }
    }
}
```

**Step 4: 验证编译**

Run: `cd rust && cargo build -p brain-hooks`
Expected: BUILD SUCCEEDED（可能有 unused warnings，正常）

**Step 5: Commit**

```bash
git add crates/brain-hooks/
git commit -m "feat(hooks): create brain-hooks crate with core types"
```

---

### Task 2: 实现 config.rs — 配置加载

**Files:**
- Create: `rust/crates/brain-hooks/src/config.rs`
- Modify: `rust/crates/brain-llm/src/config.rs` — 扩展 LlmConfig 增加 hooks 段

**Step 1: 创建 config.rs**

brain-hooks 自身的配置结构，从 `config.toml` 的 `[hooks]` 段加载。

```rust
use serde::{Deserialize, Serialize};

use crate::types::HookHandlerConfig;

/// Hook 配置段（对应 config.toml 的 [hooks]）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HooksConfig {
    /// 全局开关
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// eval_gate 内置 handler 配置
    #[serde(default)]
    pub eval_gate: EvalGateConfig,

    /// PreToolUse command handlers
    #[serde(default)]
    pub pre_tool_use: Vec<HookHandlerConfig>,

    /// PostToolUse command handlers
    #[serde(default)]
    pub post_tool_use: Vec<HookHandlerConfig>,

    /// PostQuery command handlers
    #[serde(default)]
    pub post_query: Vec<HookHandlerConfig>,

    /// OnShutdown command handlers
    #[serde(default)]
    pub on_shutdown: Vec<HookHandlerConfig>,

    /// SessionStart command handlers
    #[serde(default)]
    pub session_start: Vec<HookHandlerConfig>,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            eval_gate: EvalGateConfig::default(),
            pre_tool_use: Vec::new(),
            post_tool_use: Vec::new(),
            post_query: Vec::new(),
            on_shutdown: Vec::new(),
            session_start: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// eval_gate 内置 handler 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalGateConfig {
    /// 是否启用评估脑自决策
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 用哪个 brain_models 里的模型（"off" 禁用）
    #[serde(default = "default_eval_model")]
    pub model: String,
}

impl Default for EvalGateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: default_eval_model(),
        }
    }
}

fn default_eval_model() -> String {
    "eval".to_string()
}
```

**Step 2: 扩展 brain-llm 的 LlmConfig，增加 hooks 段**

在 `brain-llm/src/config.rs` 中给 `LlmConfig` 加一个可选的 `hooks` 字段。注意不直接依赖 brain-hooks（避免循环依赖），而是用 `toml::Value` 占位，由 ai-brain-cli 层解析。

```rust
// 在 LlmConfig 中新增：
#[serde(default)]
pub hooks: Option<toml::Value>,
```

然后 ai-brain-cli 在加载配置时，把 `hooks` 段传给 brain-hooks 的 `HooksConfig::from_toml_value()`。

**Step 3: 在 config.rs 中增加从 toml::Value 解析的方法**

```rust
impl HooksConfig {
    /// 从 toml::Value 解析
    pub fn from_toml_value(value: &toml::Value) -> Self {
        value.clone().try_into().unwrap_or_default()
    }

    /// 获取某个事件的所有 handler 配置
    pub fn handlers_for_event(&self, event: crate::types::HookEvent) -> &[HookHandlerConfig] {
        use crate::types::HookEvent;
        match event {
            HookEvent::PreToolUse => &self.pre_tool_use,
            HookEvent::PostToolUse => &self.post_tool_use,
            HookEvent::PostQuery => &self.post_query,
            HookEvent::OnShutdown => &self.on_shutdown,
            HookEvent::SessionStart => &self.session_start,
        }
    }
}
```

注意：brain-hooks 需要加 `toml = "0.8"` 依赖。

**Step 4: 更新 brain-hooks Cargo.toml，增加 toml 依赖**

在 `[dependencies]` 中加：
```toml
toml = "0.8"
```

**Step 5: 验证编译**

Run: `cd rust && cargo build -p brain-hooks`
Expected: BUILD SUCCEEDED

**Step 6: Commit**

```bash
git add crates/brain-hooks/src/config.rs crates/brain-hooks/Cargo.toml crates/brain-llm/src/config.rs
git commit -m "feat(hooks): add config loading from config.toml [hooks] section"
```

---

### Task 3: 实现 runner.rs — HookRunner 执行引擎

**Files:**
- Create: `rust/crates/brain-hooks/src/runner.rs`

这是核心执行引擎，参考 `runtime/hooks.rs` 的 command 执行模式，但事件类型更丰富。

**Step 1: 创建 runner.rs**

```rust
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::HooksConfig;
use crate::types::{HookDecision, HookEvent, HookHandlerConfig, HookInput, HookMatcher, HookOutput};

/// Hook 执行引擎
#[derive(Debug, Clone)]
pub struct HookRunner {
    config: HooksConfig,
}

impl HookRunner {
    pub fn new(config: HooksConfig) -> Self {
        Self { config }
    }

    /// 全局开关是否启用
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// 运行某个事件的所有 handler
    pub async fn run(&self, input: &HookInput) -> Vec<HookOutput> {
        if !self.config.enabled {
            return vec![HookOutput::allow()];
        }

        let handlers = self.config.handlers_for_event(input.event);
        if handlers.is_empty() {
            return vec![HookOutput::allow()];
        }

        let mut outputs = Vec::new();
        for handler_config in handlers {
            match self.execute_handler(handler_config, input).await {
                Ok(output) => {
                    outputs.push(output.clone());
                    // Deny 短路：任一 handler deny 则停止后续
                    if output.decision == HookDecision::Deny {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("Hook handler 执行失败: {e}");
                    // 失败不阻断主流程，继续下一个
                }
            }
        }

        if outputs.is_empty() {
            outputs.push(HookOutput::allow());
        }

        outputs
    }

    /// 执行单个 handler
    async fn execute_handler(
        &self,
        config: &HookHandlerConfig,
        input: &HookInput,
    ) -> Result<HookOutput, String> {
        match config {
            HookHandlerConfig::Command {
                command,
                matcher,
                timeout,
            } => {
                let matcher = HookMatcher::parse(matcher.as_deref());
                // 对工具事件做匹配检查
                if let Some(ref tool_name) = input.tool_name {
                    if !matcher.matches(tool_name) {
                        return Ok(HookOutput::allow());
                    }
                }
                self.run_command(command, *timeout, input).await
            }
            HookHandlerConfig::Builtin { name } => {
                // 内置 handler 由 builtins.rs 处理，runner 只路由
                Err(format!("未知内置 handler: {name}"))
            }
        }
    }

    /// 执行 shell command handler
    async fn run_command(
        &self,
        command: &str,
        timeout_secs: u64,
        input: &HookInput,
    ) -> Result<HookOutput, String> {
        let payload = build_payload(input);

        // 在 tokio task 中执行带超时的子进程
        let command = command.to_string();
        let event_str = input.event.as_str();

        tokio::task::spawn_blocking(move || {
            let mut child = Command::new("sh");
            child.arg("-lc").arg(&command);
            child.stdin(Stdio::piped());
            child.stdout(Stdio::piped());
            child.stderr(Stdio::piped());
            child.env("HOOK_EVENT", event_str);

            let mut child = child.spawn().map_err(|e| format!("启动失败: {e}"))?;

            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(payload.as_bytes());
            }

            // 带超时等待
            let result = child.wait_timeout(Duration::from_secs(timeout_secs))?;

            let output = match result {
                Some(status) => {
                    let output = child.wait_with_output().map_err(|e| format!("读取输出失败: {e}"))?;
                    (status.code(), output)
                }
                None => {
                    let _ = child.kill();
                    let output = child.wait_with_output().map_err(|e| format!("清理失败: {e}"))?;
                    return Err(format!("Hook `{command}` 超时 ({timeout_secs}s)"));
                }
            };

            let stdout = String::from_utf8_lossy(&output.1.stdout).trim().to_string();
            let _ = output; // 消耗

            Ok(parse_command_output(&stdout))
        })
        .await
        .map_err(|e| format!("spawn_blocking 失败: {e}"))?
    }
}

/// 构建传给 handler 的 JSON payload
fn build_payload(input: &HookInput) -> String {
    let mut payload = json!({
        "hook_event_name": input.event.as_str(),
        "session_id": input.session_id,
    });

    if let Some(ref tool_name) = input.tool_name {
        payload["tool_name"] = json!(tool_name);
    }
    if let Some(ref tool_input) = input.tool_input {
        payload["tool_input"] = json!(tool_input);
    }
    if let Some(ref tool_output) = input.tool_output {
        payload["tool_output"] = json!(tool_output);
    }
    if let Some(ref user_input) = input.user_input {
        payload["user_input"] = json!(user_input);
    }
    if let Some(ref ai_output) = input.ai_output {
        payload["ai_output"] = json!(ai_output);
    }

    payload.to_string()
}

/// 解析 command handler 的 stdout
fn parse_command_output(stdout: &str) -> HookOutput {
    if stdout.is_empty() {
        return HookOutput::allow();
    }

    // 尝试解析 JSON
    if let Ok(Value::Object(root)) = serde_json::from_str::<Value>(stdout) {
        let decision = if root.get("decision").and_then(Value::as_str) == Some("block")
            || root.get("continue").and_then(Value::as_bool) == Some(false)
        {
            HookDecision::Deny
        } else {
            HookDecision::Allow
        };

        let reason = root
            .get("reason")
            .or_else(|| root.get("systemMessage"))
            .and_then(Value::as_str)
            .map(String::from);

        let system_message = root
            .get("systemMessage")
            .and_then(Value::as_str)
            .map(String::from);

        let trigger_eval = root
            .get("trigger_eval")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        return HookOutput {
            decision,
            reason,
            trigger_eval,
            system_message,
        };
    }

    // 非 JSON：纯文本作为 system_message
    HookOutput {
        decision: HookDecision::Allow,
        reason: None,
        trigger_eval: false,
        system_message: Some(stdout.to_string()),
    }
}

/// Child process wait with timeout（辅助 trait）
trait ChildExt {
    fn wait_timeout(&mut self, duration: Duration) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl ChildExt for std::process::Child {
    fn wait_timeout(&mut self, duration: Duration) -> std::io::Result<Option<std::process::ExitStatus>> {
        let start = std::time::Instant::now();
        loop {
            match self.try_wait()? {
                Some(status) => return Ok(Some(status)),
                None => {
                    if start.elapsed() >= duration {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
}
```

注意：需要修复编译问题 — `child.wait_with_output()` 在 `try_wait` 后不可用。实际实现中应该在 spawn 后直接用 `child.wait_with_output()` 配合超时。上面的代码作为指导方向，实际实现时需要调整子进程管理逻辑（参考 runtime/hooks.rs 的 `CommandWithStdin` 模式）。

**Step 2: 验证编译**

Run: `cd rust && cargo build -p brain-hooks`
Expected: BUILD SUCCEEDED

**Step 3: Commit**

```bash
git add crates/brain-hooks/src/runner.rs
git commit -m "feat(hooks): implement HookRunner with command handler execution"
```

---

### Task 4: 实现 builtins.rs — eval_gate 内置 handler

**Files:**
- Create: `rust/crates/brain-hooks/src/builtins.rs`

这是核心创新点：用 ~200 token 的轻量 LLM 调用决定是否需要触发完整评估脑。

**Step 1: 创建 builtins.rs**

```rust
use brain_llm::{ChatMessage, ChatRequest, LlmProvider};
use std::sync::Arc;

use crate::types::{HookOutput, HookDecision};

const EVAL_GATE_SYSTEM_PROMPT: &str = r#"你是评估脑的守门员。判断以下 AI 回复是否需要深度质量评估。

需要评估的情况：
- 回复中可能包含代码bug、错误事实、偷懒行为（TODO/FIXME）
- 回复涉及用户已知的踩坑点
- 回复明显忽略了用户的部分指令

不需要评估的情况：
- 简单问答、闲聊、确认
- 用户只是在测试或探索
- 回复质量明显没问题

严格输出 JSON，不要附加其他文字：
{"need_eval": true/false, "reason": "一句话说明"}"#;

/// eval_gate 决策结果
#[derive(Debug, serde::Deserialize)]
struct EvalGateResponse {
    need_eval: bool,
    #[allow(dead_code)]
    reason: Option<String>,
}

/// 运行 eval_gate：轻量 LLM 调用判断是否需要评估
pub async fn run_eval_gate(
    llm: &Arc<dyn LlmProvider>,
    user_input: &str,
    ai_output: &str,
) -> HookOutput {
    // 截断过长的输出（节省 token）
    let ai_preview = truncate(ai_output, 500);
    let user_preview = truncate(user_input, 200);

    let user_prompt = format!(
        "## 用户输入\n{user_preview}\n\n## AI 回复\n{ai_preview}\n\n请判断是否需要深度评估。"
    );

    let request = ChatRequest {
        model: None,
        messages: vec![
            ChatMessage::system(EVAL_GATE_SYSTEM_PROMPT),
            ChatMessage::user(user_prompt),
        ],
        max_tokens: Some(100),
        temperature: Some(0.1),
        tools: None,
        tool_choice: None,
    };

    match llm.complete(request).await {
        Ok(response) => {
            let text = response.text();
            let trigger = parse_eval_gate_response(&text);
            HookOutput {
                decision: HookDecision::Allow,
                reason: None,
                trigger_eval: trigger,
                system_message: None,
            }
        }
        Err(e) => {
            tracing::warn!("eval_gate LLM 调用失败，默认不评估: {e}");
            HookOutput::allow()
        }
    }
}

fn parse_eval_gate_response(text: &str) -> bool {
    let trimmed = text.trim();

    // 尝试解析 JSON
    if let Ok(resp) = serde_json::from_str::<EvalGateResponse>(trimmed) {
        return resp.need_eval;
    }

    // 尝试提取 JSON 块
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if let Ok(resp) = serde_json::from_str::<EvalGateResponse>(&trimmed[start..=end]) {
                return resp.need_eval;
            }
        }
    }

    // 解析失败，默认不评估
    tracing::warn!("eval_gate 响应解析失败: {}", &trimmed[..trimmed.len().min(100)]);
    false
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}...(已截断)")
    }
}
```

**Step 2: 在 runner.rs 中集成 eval_gate 调用**

修改 `HookRunner` 增加一个可选的 LLM 引用，当 handler 是 `Builtin { name: "eval_gate" }` 时，路由到 `builtins::run_eval_gate()`。

在 runner.rs 中：
- `HookRunner` 新增字段 `eval_gate_llm: Option<Arc<dyn LlmProvider>>`
- 新增方法 `pub fn with_eval_gate_llm(mut self, llm: Arc<dyn LlmProvider>) -> Self`
- 在 `execute_handler` 的 `Builtin` 分支中调用 `builtins::run_eval_gate()`

**Step 3: 验证编译**

Run: `cd rust && cargo build -p brain-hooks`
Expected: BUILD SUCCEEDED

**Step 4: Commit**

```bash
git add crates/brain-hooks/src/builtins.rs crates/brain-hooks/src/runner.rs
git commit -m "feat(hooks): implement eval_gate builtin handler for PostQuery"
```

---

### Task 5: 在 LlmConfig 中增加 hooks 段

**Files:**
- Modify: `rust/crates/brain-llm/src/config.rs` — 给 `LlmConfig` 增加 `hooks` 字段

**Step 1: 修改 LlmConfig**

在 `brain-llm/src/config.rs` 的 `LlmConfig` 结构体中增加：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub llm: LlmSection,
    #[serde(default)]
    pub brain: BrainSection,
    /// Hook 系统配置（原始 toml::Value，由 ai-brain-cli 层解析为 brain-hooks::HooksConfig）
    #[serde(default)]
    pub hooks: Option<toml::Value>,
}
```

同时给 `brain-llm/Cargo.toml` 增加 `toml = "0.8"` 依赖。

**Step 2: 更新 default_config()**

在 `LlmConfig::default_config()` 中增加 `hooks: None`。

**Step 3: 验证编译**

Run: `cd rust && cargo build -p brain-llm`
Expected: BUILD SUCCEEDED

**Step 4: Commit**

```bash
git add crates/brain-llm/src/config.rs crates/brain-llm/Cargo.toml
git commit -m "feat(config): add [hooks] section to LlmConfig"
```

---

### Task 6: 集成到 orchestrator — 替换评估脑调用

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`
- Modify: `rust/crates/ai-brain-cli/Cargo.toml` — 增加 `brain-hooks` 依赖

**Step 1: ai-brain-cli/Cargo.toml 增加依赖**

```toml
brain-hooks = { path = "../brain-hooks" }
```

**Step 2: orchestrator.rs — 初始化 HookRunner**

在 `Orchestrator::new()` 中：
1. 从 `LlmConfig` 读取 `hooks` 段
2. 解析为 `HooksConfig`
3. 创建 `HookRunner`
4. 如果 eval_gate 启用且配置了 eval 模型，创建 LLM 客户端注入

```rust
// orchestrator.rs 新增字段
hook_runner: brain_hooks::runner::HookRunner,

// Orchestrator::new() 中
let hooks_config: brain_hooks::config::HooksConfig = config
    .hooks
    .as_ref()
    .map(|v| brain_hooks::config::HooksConfig::from_toml_value(v))
    .unwrap_or_default();

let mut hook_runner = brain_hooks::runner::HookRunner::new(hooks_config);

// 注入 eval_gate LLM
if hooks_config.eval_gate.enabled && hooks_config.eval_gate.model != "off" {
    if let Ok(client) = config.create_brain_client(&hooks_config.eval_gate.model) {
        hook_runner = hook_runner.with_eval_gate_llm(std::sync::Arc::from(client));
    }
}
```

**Step 3: orchestrator.rs — PostQuery hook 替换评估脑调用**

将现有的无条件评估脑调用（第 528-591 行）改为：

```rust
// 主脑回复完成后，通过 hook 系统决策是否触发评估
if let Ok(ref output) = result {
    let hook_input = brain_hooks::types::HookInput {
        event: brain_hooks::types::HookEvent::PostQuery,
        session_id: session_id.clone(),
        cwd: std::env::current_dir().unwrap_or_default(),
        tool_name: None,
        tool_input: None,
        tool_output: None,
        is_error: false,
        user_input: Some(input_owned.clone()),
        ai_output: Some(output.answer.clone()),
    };

    let hook_outputs = this.hook_runner.run(&hook_input).await;
    let should_eval = hook_outputs.iter().any(|o| o.trigger_eval);

    if should_eval {
        if let Some(ref eb) = this.eval_brain {
            // ... 现有评估脑调用逻辑（保持不变）
        }
    } else {
        tracing::info!("eval_gate 判定：跳过评估");
    }
}
```

**Step 4: orchestrator.rs — OnShutdown hook**

在 `shutdown_with_analysis()` 中增加 `OnShutdown` hook 调用。

**Step 5: 验证编译**

Run: `cd rust && cargo build -p ai-brain-cli`
Expected: BUILD SUCCEEDED

**Step 6: Commit**

```bash
git add crates/ai-brain-cli/
git commit -m "feat(orchestrator): integrate brain-hooks, replace eval with PostQuery hook"
```

---

### Task 7: 集成到 tool_loop — PreToolUse / PostToolUse

**Files:**
- Modify: `rust/crates/brain-main/src/tool_loop.rs`
- Modify: `rust/crates/brain-main/Cargo.toml` — 增加 `brain-hooks` 依赖

**Step 1: brain-main/Cargo.toml 增加依赖**

```toml
brain-hooks = { path = "../brain-hooks" }
```

**Step 2: tool_loop.rs — 注入 HookRunner**

修改 `run_tool_loop_with_config` 函数签名，增加 `hook_runner: Option<&brain_hooks::runner::HookRunner>` 参数。

**Step 3: PreToolUse hook**

在 `execute_tool_calls` 中，工具执行前（第 182 行 `guard_check` 之后）：

```rust
// guard_check 之后、实际执行之前
if let Some(runner) = hook_runner {
    let hook_input = brain_hooks::types::HookInput {
        event: brain_hooks::types::HookEvent::PreToolUse,
        session_id: String::new(), // 从上下文获取
        cwd: std::env::current_dir().unwrap_or_default(),
        tool_name: Some(name.clone()),
        tool_input: Some(serde_json::to_string(input).unwrap_or_default()),
        tool_output: None,
        is_error: false,
        user_input: None,
        ai_output: None,
    };
    let outputs = runner.run(&hook_input).await;
    if outputs.iter().any(|o| o.decision == brain_hooks::types::HookDecision::Deny) {
        let reason = outputs.iter().find_map(|o| o.reason.clone())
            .unwrap_or_else(|| "PreToolUse hook denied".into());
        messages.push(ChatMessage::tool_result(id, format!("Hook 拒绝: {reason}"), true));
        // 发送 ToolDone 进度事件
        continue;
    }
}
```

**Step 4: PostToolUse hook**

在工具执行成功后（第 234 行 `messages.push` 之后）：

```rust
if let Some(runner) = hook_runner {
    let hook_input = brain_hooks::types::HookInput {
        event: brain_hooks::types::HookEvent::PostToolUse,
        session_id: String::new(),
        cwd: std::env::current_dir().unwrap_or_default(),
        tool_name: Some(name.clone()),
        tool_input: Some(serde_json::to_string(input).unwrap_or_default()),
        tool_output: Some(result.output.chars().take(200).collect()),
        is_error: result.is_error,
        user_input: None,
        ai_output: None,
    };
    let _ = runner.run(&hook_input).await;
}
```

**Step 5: 验证编译**

Run: `cd rust && cargo build -p brain-main`
Expected: BUILD SUCCEEDED

**Step 6: Commit**

```bash
git add crates/brain-main/
git commit -m "feat(tool_loop): integrate PreToolUse/PostToolUse hooks"
```

---

### Task 8: 测试 + 验证

**Files:**
- Create: `rust/crates/brain-hooks/src/runner.rs` 中的 `#[cfg(test)]` 模块
- Create: `rust/crates/brain-hooks/src/builtins.rs` 中的 `#[cfg(test)]` 模块

**Step 1: types.rs 测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_parse_match_all() {
        assert_eq!(Matcher::parse(None), Matcher::MatchAll);
        assert_eq!(Matcher::parse(Some("")), Matcher::MatchAll);
        assert_eq!(Matcher::parse(Some("*")), Matcher::MatchAll);
    }

    #[test]
    fn matcher_parse_pipe_delimited() {
        let m = Matcher::parse(Some("Bash|Edit|Write"));
        assert!(m.matches("Bash"));
        assert!(m.matches("Edit"));
        assert!(!m.matches("Read"));
    }

    #[test]
    fn hook_event_as_str() {
        assert_eq!(HookEvent::PreToolUse.as_str(), "PreToolUse");
        assert_eq!(HookEvent::PostQuery.as_str(), "PostQuery");
    }

    #[test]
    fn hook_output_allow_deny() {
        let allow = HookOutput::allow();
        assert_eq!(allow.decision, HookDecision::Allow);
        assert!(!allow.trigger_eval);

        let deny = HookOutput::deny("test".into());
        assert_eq!(deny.decision, HookDecision::Deny);
    }
}
```

**Step 2: config.rs 测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_enabled() {
        let config = HooksConfig::default();
        assert!(config.enabled);
        assert!(config.eval_gate.enabled);
        assert!(config.pre_tool_use.is_empty());
    }

    #[test]
    fn parse_from_toml() {
        let toml_str = r#"
enabled = true

[hooks.eval_gate]
enabled = false

[[hooks.pre_tool_use]]
type = "command"
command = "echo test"
timeout = 10
"#;
        let value: toml::Value = toml::from_str(toml_str).unwrap();
        // 注意：这里测试的是嵌套在 [hooks] 下的结构
        // 实际解析时 hooks 段本身就是这个 value
    }
}
```

**Step 3: builtins.rs 测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_eval_gate_true() {
        let json = r#"{"need_eval": true, "reason": "代码可能有 bug"}"#;
        assert!(parse_eval_gate_response(json));
    }

    #[test]
    fn parse_eval_gate_false() {
        let json = r#"{"need_eval": false, "reason": "简单问答"}"#;
        assert!(!parse_eval_gate_response(json));
    }

    #[test]
    fn parse_eval_gate_with_prefix() {
        let text = "评估结果如下：\n{\"need_eval\": true, \"reason\": \"test\"}";
        assert!(parse_eval_gate_response(text));
    }

    #[test]
    fn parse_eval_gate_invalid_defaults_false() {
        assert!(!parse_eval_gate_response("not json"));
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let long: String = "a".repeat(100);
        let result = truncate(&long, 50);
        assert!(result.ends_with("...(已截断)"));
    }
}
```

**Step 4: 运行全部测试**

Run: `cd rust && cargo test -p brain-hooks`
Expected: ALL TESTS PASS

**Step 5: 运行 workspace 测试确认无回归**

Run: `cd rust && cargo test --workspace --exclude brain-integration-tests`
Expected: ALL TESTS PASS（~230 tests）

**Step 6: Commit**

```bash
git add crates/brain-hooks/
git commit -m "test(hooks): add unit tests for types, config, builtins"
```

---

### Task 9: 清理 + 文档更新

**Files:**
- Modify: `rust/crates/brain-hooks/src/lib.rs` — 确认所有 pub mod 正确
- Update: 项目 MEMORY.md — 记录 hook 系统架构

**Step 1: 确认 lib.rs 导出**

```rust
pub mod builtins;
pub mod config;
pub mod runner;
pub mod types;
```

**Step 2: 编译检查**

Run: `cd rust && cargo build -p brain-hooks && cargo build -p ai-brain-cli`
Expected: BUILD SUCCEEDED

**Step 3: Commit**

```bash
git add -A
git commit -m "feat(hooks): complete brain-hooks system with eval_gate integration"
```
