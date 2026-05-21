# 斜杠命令系统实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 为 AI Brain TUI 添加 Claude Code 风格的冒号命令交互系统，包含下拉命令面板、两级命令结构、动态补全、混合执行模式。

**Architecture:** 新建 `command/` 模块包含命令注册表 `CommandRegistry` 和各类命令处理器。新建 `tui/command_panel.rs` 作为下拉面板 UI 组件。修改 `Orchestrator` 结构体保存 `PluginManager`/`SkillCatalog`/`McpClientPool` 的 `Arc` 引用，以便命令处理器调用。替换 `EvolutionCompleter` 硬编码补全为从 `CommandRegistry` 动态生成。

**Tech Stack:** Rust, ratatui, crossterm, tokio, Arc<Mutex>

---

### Task 1: CommandRegistry 核心类型

**Files:**
- Create: `rust/crates/ai-brain-cli/src/command/mod.rs`
- Create: `rust/crates/ai-brain-cli/src/command/registry.rs`
- Test: `rust/crates/ai-brain-cli/src/command/registry.rs` (内联 #[cfg(test)])

**Step 1: 写失败测试**

```rust
// command/registry.rs 底部
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_empty() {
        let reg = CommandRegistry::new();
        assert!(reg.commands.is_empty());
        assert!(reg.find_command("help").is_none());
    }

    #[test]
    fn test_register_and_find() {
        let mut reg = CommandRegistry::new();
        reg.register(Command {
            name: "help",
            description: "帮助",
            group: CommandGroup::BuiltIn,
            subcommands: vec![],
            handler: CommandHandler::Sync(|_, _| CommandResult::ok("ok")),
        });
        assert!(reg.find_command("help").is_some());
        assert!(reg.find_command("nonexist").is_none());
    }

    #[test]
    fn test_filter_by_prefix() {
        let mut reg = CommandRegistry::new();
        reg.register(Command {
            name: "help",
            description: "帮助",
            group: CommandGroup::BuiltIn,
            subcommands: vec![],
            handler: CommandHandler::Sync(|_, _| CommandResult::ok("ok")),
        });
        reg.register(Command {
            name: "status",
            description: "状态",
            group: CommandGroup::BuiltIn,
            subcommands: vec![],
            handler: CommandHandler::Sync(|_, _| CommandResult::ok("ok")),
        });
        let results = reg.filter_commands("he");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "help");
    }

    #[test]
    fn test_find_subcommand() {
        let mut reg = CommandRegistry::new();
        reg.register(Command {
            name: "plugin",
            description: "插件",
            group: CommandGroup::Plugin,
            subcommands: vec![
                SubCommand { name: "list", description: "列出", args: vec![] },
                SubCommand { name: "install", description: "安装", args: vec![
                    ArgSpec { name: "path", required: true, completer: None },
                ] },
            ],
            handler: CommandHandler::Sync(|_, _| CommandResult::ok("ok")),
        });
        let cmd = reg.find_command("plugin").unwrap();
        assert_eq!(cmd.subcommands.len(), 2);
        let sub = reg.find_subcommand(cmd, "list");
        assert!(sub.is_some());
    }
}
```

**Step 2: 运行测试验证失败**

Run: `cd rust && cargo test --package ai-brain-cli command::registry::tests -- --nocapture 2>&1 | head -20`
Expected: 编译失败，模块不存在

**Step 3: 写最小实现**

```rust
// command/mod.rs
pub mod registry;

pub use registry::{Command, CommandGroup, CommandHandler, CommandRegistry, CommandResult, SubCommand, ArgSpec};
```

```rust
// command/registry.rs
//! 命令注册表 — 统一管理所有冒号命令

/// 命令定义
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
    pub group: CommandGroup,
    pub subcommands: Vec<SubCommand>,
    pub handler: CommandHandler,
}

/// 子命令定义
pub struct SubCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub args: Vec<ArgSpec>,
}

/// 参数定义
pub struct ArgSpec {
    pub name: &'static str,
    pub required: bool,
    pub completer: Option<fn() -> Vec<String>>,
}

/// 命令分组
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandGroup {
    BuiltIn,
    Plugin,
    Skill,
    Mcp,
    Memory,
    Evolver,
    Config,
}

impl CommandGroup {
    pub fn label(&self) -> &'static str {
        match self {
            Self::BuiltIn => "内置",
            Self::Plugin => "插件",
            Self::Skill => "技能",
            Self::Mcp => "MCP",
            Self::Memory => "记忆",
            Self::Evolver => "进化",
            Self::Config => "配置",
        }
    }
}

/// 命令处理器（同步/异步）
pub enum CommandHandler {
    Sync(fn(&[String]) -> CommandResult),
    Async(fn(&[String]) -> CommandResult),
}

/// 命令执行结果
pub struct CommandResult {
    pub output: String,
    pub success: bool,
}

impl CommandResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self { output: output.into(), success: true }
    }
    pub fn err(output: impl Into<String>) -> Self {
        Self { output: output.into(), success: false }
    }
}

/// 命令注册表
pub struct CommandRegistry {
    pub commands: Vec<Command>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self { commands: Vec::new() }
    }

    pub fn register(&mut self, cmd: Command) {
        self.commands.push(cmd);
    }

    pub fn find_command(&self, name: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.name == name)
    }

    pub fn find_subcommand<'a>(&self, cmd: &'a Command, name: &str) -> Option<&'a SubCommand> {
        cmd.subcommands.iter().find(|s| s.name == name)
    }

    /// 按前缀过滤命令
    pub fn filter_commands(&self, prefix: &str) -> Vec<&Command> {
        self.commands
            .iter()
            .filter(|c| c.name.starts_with(prefix))
            .collect()
    }

    /// 按前缀过滤子命令
    pub fn filter_subcommands<'a>(&self, cmd: &'a Command, prefix: &str) -> Vec<&'a SubCommand> {
        cmd.subcommands
            .iter()
            .filter(|s| s.name.starts_with(prefix))
            .collect()
    }
}
```

**Step 4: 注册模块到 main.rs**

在 `rust/crates/ai-brain-cli/src/main.rs` 顶部添加:
```rust
mod command;
```

**Step 5: 运行测试验证通过**

Run: `cd rust && cargo test --package ai-brain-cli command::registry::tests -- --nocapture`
Expected: 4 tests passed

**Step 6: 提交**

```bash
git add rust/crates/ai-brain-cli/src/command/ rust/crates/ai-brain-cli/src/main.rs
git commit -m "feat: 添加 CommandRegistry 核心类型和测试"
```

---

### Task 2: 内置命令 (help/status/quit/clear)

**Files:**
- Create: `rust/crates/ai-brain-cli/src/command/builtin.rs`
- Modify: `rust/crates/ai-brain-cli/src/command/mod.rs` — 添加 builtin 模块

**Step 1: 写失败测试**

```rust
// command/builtin.rs 底部
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_all() {
        let reg = register_builtin();
        assert!(reg.find_command("help").is_some());
        assert!(reg.find_command("status").is_some());
        assert!(reg.find_command("quit").is_some());
        assert!(reg.find_command("clear").is_some());
    }

    #[test]
    fn test_help_output() {
        let reg = register_builtin();
        let cmd = reg.find_command("help").unwrap();
        if let CommandHandler::Sync(f) = &cmd.handler {
            let result = f(&[]);
            assert!(result.output.contains("help"));
            assert!(result.success);
        } else {
            panic!("help should be sync");
        }
    }
}
```

**Step 2: 运行测试验证失败**

Run: `cd rust && cargo test --package ai-brain-cli command::builtin::tests -- --nocapture 2>&1 | head -20`
Expected: 编译失败

**Step 3: 写实现**

```rust
// command/builtin.rs
//! 内置命令: help, status, quit, clear

use super::registry::*;

/// 注册所有内置命令
pub fn register_builtin() -> CommandRegistry {
    let mut reg = CommandRegistry::new();
    reg.register(Command {
        name: "help",
        description: "显示所有命令帮助",
        group: CommandGroup::BuiltIn,
        subcommands: vec![],
        handler: CommandHandler::Sync(cmd_help),
    });
    reg.register(Command {
        name: "status",
        description: "系统状态",
        group: CommandGroup::BuiltIn,
        subcommands: vec![],
        handler: CommandHandler::Sync(|_| CommandResult::ok("（状态信息由 TUI 层填充）")),
    });
    reg.register(Command {
        name: "quit",
        description: "退出",
        group: CommandGroup::BuiltIn,
        subcommands: vec![],
        handler: CommandHandler::Sync(|_| CommandResult::ok("__QUIT__")),
    });
    reg.register(Command {
        name: "clear",
        description: "清空输出",
        group: CommandGroup::BuiltIn,
        subcommands: vec![],
        handler: CommandHandler::Sync(|_| CommandResult::ok("__CLEAR__")),
    });
    reg
}

fn cmd_help(_args: &[String]) -> CommandResult {
    CommandResult::ok(
        "=== AI Brain v2 命令 ===\n\
         :help           — 显示帮助\n\
         :status         — 系统状态\n\
         :clear          — 清空输出\n\
         :quit           — 退出\n\
         :plugin list/install/uninstall/reload\n\
         :skill list/run/info\n\
         :mcp list/status/reconnect\n\
         :memory stats/recall/save/daily\n\
         :evo <目标>/status/approve/reject/diff\n\
         :config get/set/brain-params"
    )
}
```

更新 `command/mod.rs`:
```rust
pub mod builtin;
pub mod registry;

pub use registry::*;
```

**Step 4: 运行测试验证通过**

Run: `cd rust && cargo test --package ai-brain-cli command::builtin::tests -- --nocapture`
Expected: 2 tests passed

**Step 5: 提交**

```bash
git add rust/crates/ai-brain-cli/src/command/
git commit -m "feat: 添加内置命令 (help/status/quit/clear)"
```

---

### Task 3: Orchestrator 保存 Plugin/Skill/MCP 引用

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`
  - `Orchestrator` struct: 添加 `plugin_mgr`, `skill_catalog`, `mcp_pool` 字段
  - `create_v2_brain()`: 返回这些组件
  - `Orchestrator::new()`: 存储到结构体

**Step 1: 写失败测试**

```rust
// orchestrator.rs 底部（或新增 tests 模块）
// 由于 Orchestrator::new() 需要完整环境，此 Task 只验证编译通过
```

此 Task 不添加独立测试，只需确保 `cargo check` 通过。

**Step 2: 修改 Orchestrator 结构体**

在 `Orchestrator` struct 中添加三个字段（约在 144-170 行之间）:

```rust
pub struct Orchestrator {
    // ... 现有字段 ...

    /// 插件管理器
    plugin_mgr: Option<PluginManager>,
    /// 技能目录
    skill_catalog: Arc<SkillCatalog>,
    /// MCP 客户端池
    mcp_pool: Arc<McpClientPool>,
}
```

**Step 3: 修改 create_v2_brain 返回值**

当前签名:
```rust
fn create_v2_brain(...) -> Result<Arc<Mutex<Option<MainBrain>>>, String>
```

改为返回元组:
```rust
fn create_v2_brain(...) -> Result<(Arc<Mutex<Option<MainBrain>>>, Option<PluginManager>, Arc<SkillCatalog>, Arc<McpClientPool>), String>
```

在函数末尾:
```rust
Ok((Arc::new(Mutex::new(Some(brain))), plugin_mgr, skill_catalog, mcp_pool))
```

同时需要新建 `McpClientPool` 实例并保存（当前在 `RealToolExecutor::with_mcp_pool` 中创建后丢弃）:
```rust
let mcp_pool = Arc::new(McpClientPool::new());
// ... 使用 mcp_pool.clone() 传给 RealToolExecutor ...
```

**Step 4: 修改 Orchestrator::new() 构造**

在 `Orchestrator::new()` 中接收返回值并赋给结构体字段:
```rust
let (v2_brain, plugin_mgr, skill_catalog, mcp_pool) = create_v2_brain(
    client.clone(), memory_brain.clone(), dispatch.clone()
)?;

// ... 构造 Orchestrator ...
Self {
    // ... 现有字段 ...
    plugin_mgr,
    skill_catalog,
    mcp_pool,
}
```

**Step 5: 暴露公共访问方法**

```rust
impl Orchestrator {
    pub fn plugin_mgr(&self) -> Option<&PluginManager> {
        self.plugin_mgr.as_ref()
    }

    pub fn skill_catalog(&self) -> &SkillCatalog {
        &self.skill_catalog
    }

    pub fn mcp_pool(&self) -> &McpClientPool {
        &self.mcp_pool
    }
}
```

**Step 6: 运行 cargo check 验证编译**

Run: `cd rust && cargo check --package ai-brain-cli 2>&1 | tail -5`
Expected: 编译通过

**Step 7: 提交**

```bash
git add rust/crates/ai-brain-cli/src/orchestrator.rs
git commit -m "feat: Orchestrator 保存 Plugin/Skill/MCP Arc 引用"
```

---

### Task 4: 配置命令 (config get/set/brain-params)

**Files:**
- Create: `rust/crates/ai-brain-cli/src/command/config_cmd.rs`

**Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_config() {
        let reg = register_config();
        let cmd = reg.find_command("config").unwrap();
        assert_eq!(cmd.subcommands.len(), 3);
    }
}
```

**Step 2: 写实现**

```rust
// command/config_cmd.rs
//! 配置命令: config get/set/brain-params

use super::registry::*;

pub fn register_config() -> CommandRegistry {
    let mut reg = CommandRegistry::new();
    reg.register(Command {
        name: "config",
        description: "配置管理",
        group: CommandGroup::Config,
        subcommands: vec![
            SubCommand { name: "get", description: "查看配置项", args: vec![
                ArgSpec { name: "key", required: true, completer: None },
            ]},
            SubCommand { name: "set", description: "修改配置项", args: vec![
                ArgSpec { name: "key", required: true, completer: None },
                ArgSpec { name: "value", required: true, completer: None },
            ]},
            SubCommand { name: "brain-params", description: "查看各脑参数", args: vec![] },
        ],
        handler: CommandHandler::Sync(handle_config),
    });
    reg
}

fn handle_config(args: &[String]) -> CommandResult {
    match args.first().map(|s| s.as_str()) {
        Some("get") => {
            if args.len() < 2 {
                return CommandResult::err("用法: :config get <key>");
            }
            // 实际读取配置由 TUI 层通过 Orchestrator 完成
            // 这里只做参数校验和路由
            CommandResult::ok(format!("（查询配置 {} — 由 TUI 层填充）", args[1]))
        }
        Some("set") => {
            if args.len() < 3 {
                return CommandResult::err("用法: :config set <key> <value>");
            }
            CommandResult::ok(format!("（设置 {} = {} — 由 TUI 层填充）", args[1], args[2]))
        }
        Some("brain-params") => {
            CommandResult::ok("（脑参数 — 由 TUI 层填充）")
        }
        _ => CommandResult::err("用法: :config get <key> | set <key> <value> | brain-params"),
    }
}
```

**Step 3: 运行测试验证通过**

Run: `cd rust && cargo test --package ai-brain-cli command::config_cmd::tests -- --nocapture`
Expected: PASS

**Step 4: 更新 mod.rs 导出**

```rust
pub mod builtin;
pub mod config_cmd;
pub mod registry;

pub use registry::*;
```

**Step 5: 提交**

```bash
git add rust/crates/ai-brain-cli/src/command/
git commit -m "feat: 添加配置命令 (config get/set/brain-params)"
```

---

### Task 5: 插件/Skill/MCP/记忆/进化命令注册

**Files:**
- Create: `rust/crates/ai-brain-cli/src/command/plugin_cmd.rs`
- Create: `rust/crates/ai-brain-cli/src/command/skill_cmd.rs`
- Create: `rust/crates/ai-brain-cli/src/command/mcp_cmd.rs`
- Create: `rust/crates/ai-brain-cli/src/command/memory_cmd.rs`
- Create: `rust/crates/ai-brain-cli/src/command/evolver_cmd.rs`

每个文件的结构与 config_cmd.rs 类似:
- `register_xxx() -> CommandRegistry`
- 处理函数做参数校验和路由
- 实际调用 Orchestrator 的逻辑在 Task 8 集成

**plugin_cmd.rs** — `:plugin list/install/uninstall/reload`
**skill_cmd.rs** — `:skill list/run/info`
**mcp_cmd.rs** — `:mcp list/status/reconnect`
**memory_cmd.rs** — `:memory stats/recall/save/daily`
**evolver_cmd.rs** — `:evo/status/approve/reject/diff`（注意 evo 的主命令无子命令形式，直接 `:evo <目标>`）

**Step 1: 写各文件的测试和空实现**

每个文件都包含:
- `register_xxx()` 返回注册了子命令的 `CommandRegistry`
- 处理函数只做参数校验
- `#[cfg(test)] mod tests { ... }` 验证注册正确

**Step 2: 更新 mod.rs**

```rust
pub mod builtin;
pub mod config_cmd;
pub mod evolver_cmd;
pub mod mcp_cmd;
pub mod memory_cmd;
pub mod plugin_cmd;
pub mod registry;
pub mod skill_cmd;

pub use registry::*;
```

**Step 3: 运行全量测试**

Run: `cd rust && cargo test --package ai-brain-cli -- --nocapture 2>&1 | tail -10`
Expected: 所有测试通过

**Step 4: 提交**

```bash
git add rust/crates/ai-brain-cli/src/command/
git commit -m "feat: 添加 plugin/skill/mcp/memory/evolver 命令注册"
```

---

### Task 6: 统一命令注册 — 组装全部命令

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/command/registry.rs` — 添加 `CommandRegistry::merge()`

**Step 1: 写测试**

```rust
#[test]
fn test_merge_registries() {
    let mut r1 = CommandRegistry::new();
    r1.register(Command {
        name: "help", description: "h", group: CommandGroup::BuiltIn,
        subcommands: vec![], handler: CommandHandler::Sync(|_| CommandResult::ok("")),
    });
    let mut r2 = CommandRegistry::new();
    r2.register(Command {
        name: "plugin", description: "p", group: CommandGroup::Plugin,
        subcommands: vec![], handler: CommandHandler::Sync(|_| CommandResult::ok("")),
    });
    r1.merge(r2);
    assert_eq!(r1.commands.len(), 2);
}
```

**Step 2: 添加 merge 方法**

```rust
impl CommandRegistry {
    pub fn merge(&mut self, other: CommandRegistry) {
        self.commands.extend(other.commands);
    }
}
```

**Step 3: 添加 all_commands() 方法**

```rust
/// 获取所有顶级命令（用于补全面板）
pub fn all_commands(&self) -> &[Command] {
    &self.commands
}
```

**Step 4: 添加 build_full_registry() 工厂函数到 mod.rs**

```rust
// command/mod.rs
pub fn build_full_registry() -> CommandRegistry {
    let mut reg = builtin::register_builtin();
    reg.merge(config_cmd::register_config());
    reg.merge(plugin_cmd::register_plugin());
    reg.merge(skill_cmd::register_skill());
    reg.merge(mcp_cmd::register_mcp());
    reg.merge(memory_cmd::register_memory());
    reg.merge(evolver_cmd::register_evolver());
    reg
}
```

**Step 5: 运行测试**

Run: `cd rust && cargo test --package ai-brain-cli command:: -- --nocapture`
Expected: 全部通过

**Step 6: 提交**

```bash
git add rust/crates/ai-brain-cli/src/command/
git commit -m "feat: 统一命令注册表合并机制"
```

---

### Task 7: 命令面板 UI 组件

**Files:**
- Create: `rust/crates/ai-brain-cli/src/tui/command_panel.rs`
- Modify: `rust/crates/ai-brain-cli/src/tui/mod.rs` — 添加模块

**Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_panel_initial_state() {
        let panel = CommandPanel::new();
        assert!(!panel.visible);
        assert!(panel.items.is_empty());
    }

    #[test]
    fn test_filter_commands() {
        let mut panel = CommandPanel::new();
        let reg = build_test_registry();
        panel.update_filter(&reg, "he");
        assert!(panel.visible);
        assert!(panel.items.iter().any(|i| i.command_name == "help"));
    }

    #[test]
    fn test_navigation() {
        let mut panel = CommandPanel::new();
        panel.items = vec![
            PanelItem { command_name: "a".into(), subcommand_name: None, description: "".into(), display: ":a".into(), group: CommandGroup::BuiltIn },
            PanelItem { command_name: "b".into(), subcommand_name: None, description: "".into(), display: ":b".into(), group: CommandGroup::BuiltIn },
        ];
        panel.visible = true;
        panel.selected = 0;
        panel.move_down();
        assert_eq!(panel.selected, 1);
        panel.move_down(); // wrap
        assert_eq!(panel.selected, 0);
        panel.move_up(); // wrap back
        assert_eq!(panel.selected, 1);
    }

    #[test]
    fn test_clear() {
        let mut panel = CommandPanel::new();
        panel.items = vec![PanelItem { command_name: "a".into(), subcommand_name: None, description: "".into(), display: ":a".into(), group: CommandGroup::BuiltIn }];
        panel.visible = true;
        panel.selected = 0;
        panel.clear();
        assert!(!panel.visible);
        assert!(panel.items.is_empty());
    }
}
```

**Step 2: 写实现**

```rust
// tui/command_panel.rs
//! 下拉命令面板 UI 组件

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};
use ratatui::Frame;

use crate::command::{CommandGroup, CommandRegistry};

/// 面板候选项
#[derive(Debug, Clone)]
pub struct PanelItem {
    pub command_name: String,
    pub subcommand_name: Option<String>,
    pub description: String,
    pub display: String,
    pub group: CommandGroup,
}

/// 命令面板状态
pub struct CommandPanel {
    pub visible: bool,
    pub items: Vec<PanelItem>,
    pub selected: usize,
    /// 当前过滤阶段
    pub phase: PanelPhase,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PanelPhase {
    /// 正在筛选顶级命令
    Command,
    /// 正在筛选子命令
    SubCommand { command_name: String },
    /// 正在输入参数
    Args { command_name: String, subcommand_name: String },
}

impl CommandPanel {
    pub fn new() -> Self {
        Self {
            visible: false,
            items: Vec::new(),
            selected: 0,
            phase: PanelPhase::Command,
        }
    }

    /// 根据输入文本更新过滤结果
    pub fn update_filter(&mut self, registry: &CommandRegistry, input: &str) {
        let input = input.trim_start_matches(':');

        match &self.phase {
            PanelPhase::Command => {
                // 按前缀过滤顶级命令
                let parts: Vec<&str> = input.splitn(2, ' ').collect();
                let cmd_prefix = parts[0];
                let rest = parts.get(1).copied();

                if let Some(rest) = rest {
                    // 已经输入了空格，切换到子命令模式
                    if let Some(cmd) = registry.find_command(cmd_prefix) {
                        self.phase = PanelPhase::SubCommand { command_name: cmd_prefix.to_string() };
                        self.filter_subcommands(registry, cmd_prefix, rest);
                        return;
                    }
                }

                let matches: Vec<PanelItem> = registry
                    .filter_commands(cmd_prefix)
                    .iter()
                    .map(|cmd| PanelItem {
                        command_name: cmd.name.to_string(),
                        subcommand_name: None,
                        description: cmd.description.to_string(),
                        display: format!(":{}", cmd.name),
                        group: cmd.group.clone(),
                    })
                    .collect();

                self.items = matches;
                self.visible = !self.items.is_empty();
                self.selected = 0;
            }
            PanelPhase::SubCommand { command_name } => {
                let rest = input.strip_prefix(command_name.as_str())
                    .and_then(|s| s.trim_start().strip_suffix(' ').unwrap_or(s).trim_start());
                self.filter_subcommands(registry, command_name, rest.unwrap_or(""));
            }
            PanelPhase::Args { .. } => {
                // 参数阶段不显示过滤面板
                self.visible = false;
            }
        }
    }

    fn filter_subcommands(&mut self, registry: &CommandRegistry, cmd_name: &str, prefix: &str) {
        if let Some(cmd) = registry.find_command(cmd_name) {
            let matches: Vec<PanelItem> = registry
                .filter_subcommands(cmd, prefix)
                .iter()
                .map(|sub| PanelItem {
                    command_name: cmd_name.to_string(),
                    subcommand_name: Some(sub.name.to_string()),
                    description: sub.description.to_string(),
                    display: format!(":{} {}", cmd_name, sub.name),
                    group: cmd.group.clone(),
                })
                .collect();
            self.items = matches;
            self.visible = !self.items.is_empty();
            self.selected = 0;
        }
    }

    pub fn move_down(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    pub fn move_up(&mut self) {
        if !self.items.is_empty() {
            self.selected = if self.selected == 0 {
                self.items.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.selected = 0;
        self.visible = false;
        self.phase = PanelPhase::Command;
    }

    /// 确认选择，返回选中的项
    pub fn confirm(&self) -> Option<&PanelItem> {
        if self.items.is_empty() {
            return None;
        }
        Some(&self.items[self.selected])
    }

    /// 渲染下拉面板
    pub fn render(&self, frame: &mut Frame, input_area: Rect) {
        if !self.visible || self.items.is_empty() {
            return;
        }

        let max_visible = 6.min(self.items.len());
        let panel_height = max_visible as u16 + 2; // +2 for border

        // 面板位置：在输入框正上方
        let panel_y = input_area.y.saturating_sub(panel_height);
        let panel_width = (input_area.width).min(50);
        let panel_area = Rect {
            x: input_area.x,
            y: panel_y,
            width: panel_width,
            height: panel_height,
        };

        let items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let style = if i == self.selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let line = Line::from(vec![
                    Span::styled(&item.display, style),
                    Span::styled(" — ", Style::default().fg(Color::DarkGray)),
                    Span::styled(item.group.label(), Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).style(Style::default().bg(Color::Rgb(30, 30, 40))));

        // 先清除面板区域，再渲染
        frame.render_widget(Clear, panel_area);
        frame.render_widget(list, panel_area);
    }
}
```

更新 `tui/mod.rs`:
```rust
pub mod command_panel;
```

**Step 3: 运行测试**

Run: `cd rust && cargo test --package ai-brain-cli tui::command_panel::tests -- --nocapture`
Expected: 4 tests passed

**Step 4: 提交**

```bash
git add rust/crates/ai-brain-cli/src/tui/command_panel.rs rust/crates/ai-brain-cli/src/tui/mod.rs
git commit -m "feat: 添加命令面板 UI 组件"
```

---

### Task 8: 集成命令面板到 TUI App

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/tui/app.rs`
  - App struct 添加 `command_registry`, `command_panel` 字段
  - `handle_key()` 检测 `:` 开头时激活命令面板
  - `handle_builtin_command_sync()` 替换为从 CommandRegistry 查找执行
  - 上下键在命令面板激活时改为面板导航
- Modify: `rust/crates/ai-brain-cli/src/tui/input.rs`
  - `update_completion()` 检测 `:` 前缀时从 CommandPanel 过滤
  - Tab 键在命令面板激活时确认选择

**Step 1: 修改 App struct**

在 `app.rs` 中添加:
```rust
use crate::command::{self, CommandRegistry, CommandResult};
use super::command_panel::CommandPanel;

// App struct 新增字段:
pub struct App {
    // ... 现有字段 ...
    command_registry: CommandRegistry,
    command_panel: CommandPanel,
}
```

在 `App::new()` 中:
```rust
let command_registry = command::build_full_registry();
let command_panel = CommandPanel::new();

Self {
    // ... 现有字段 ...
    command_registry,
    command_panel,
}
```

**Step 2: 修改输入处理 — 激活命令面板**

在 `input.rs` 的 `apply_key()` 中，当输入 `:` 字符时自动激活命令面板:

```rust
(KeyCode::Char(':'), KeyModifiers::NONE) => {
    // 输入冒号时激活命令面板
    // 注意：':' 是普通字符输入，已在 Char(c) 分支处理
    // 命令面板的激活逻辑在 update_completion 中处理
    // 这里不需要额外代码，':' 作为普通字符输入即可
}
```

实际的激活在 `update_completion()` 中处理（见下一步）。

**Step 3: 替换补全逻辑**

在 `input.rs` 的 `update_completion()` 方法中，检测到 `:` 开头时直接驱动 CommandPanel:

```rust
fn update_completion(&mut self) {
    let ctx = InputContext::analyze(&self.original, self.cursor_byte);
    self.update_hint(&ctx);

    // 不在弹出窗口模式时，只更新 hint
    if !self.popup.visible {
        return;
    }

    let items = self.completer.complete(&ctx, &self.history);
    if items.is_empty() {
        self.popup.clear();
    } else {
        self.popup.set_items(items);
    }
}
```

这段逻辑暂时保持不变（CommandPanel 是独立的 UI 组件，不替代 CompletionPopup）。

**Step 4: 替换 handle_builtin_command_sync**

将 `handle_builtin_command_sync` 改为从 `CommandRegistry` 查找和执行:

```rust
fn handle_builtin_command_sync(&mut self, input: &str) -> CommandResult {
    let trimmed = input.trim();

    // 解析命令
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.is_empty() {
        return CommandResult::Unknown;
    }

    let cmd_name = parts[0].trim_start_matches(':');
    let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

    // 查找命令
    let Some(cmd) = self.command_registry.find_command(cmd_name) else {
        return CommandResult::Unknown;
    };

    // 特殊处理：status 需要调用 Orchestrator
    if cmd_name == "status" {
        self.output.push_system(&self.orch.status());
        return CommandResult::Handled;
    }

    // 特殊处理：quit/exit
    if cmd_name == "quit" || cmd_name == "exit" {
        return CommandResult::Exit;
    }

    // 特殊处理：clear
    if cmd_name == "clear" {
        self.output.clear();
        return CommandResult::Handled;
    }

    // 通用执行
    match &cmd.handler {
        CommandHandler::Sync(f) => {
            let result = f(&args);
            if result.output == "__QUIT__" {
                return CommandResult::Exit;
            }
            if result.output == "__CLEAR__" {
                self.output.clear();
                return CommandResult::Handled;
            }
            self.output.push_system(&result.output);
            CommandResult::Handled
        }
        CommandHandler::Async(_) => {
            // 异步命令在 TUI 中暂显示提示
            self.output.push_system(&format!("命令 :{} 正在执行...", cmd_name));
            CommandResult::Handled
        }
    }
}
```

**Step 5: 命令面板按键路由**

在 `App::handle_key()` 中添加命令面板优先处理:

```rust
// 在 handle_key 最前面添加
if self.command_panel.visible {
    match key.code {
        KeyCode::Down => {
            self.command_panel.move_down();
            return;
        }
        KeyCode::Up => {
            self.command_panel.move_up();
            return;
        }
        KeyCode::Tab | KeyCode::Enter => {
            if let Some(item) = self.command_panel.confirm() {
                // 选中命令/子命令，替换输入区内容
                let replacement = match &self.command_panel.phase {
                    PanelPhase::Command => {
                        if item.subcommand_name.is_some() {
                            item.display.clone() + " "
                        } else if self.command_registry.find_command(&item.command_name)
                            .map_or(false, |c| !c.subcommands.is_empty())
                        {
                            item.display.clone() + " "
                        } else {
                            item.display.clone()
                        }
                    }
                    PanelPhase::SubCommand { .. } => {
                        item.display.clone() + " "
                    }
                    PanelPhase::Args { .. } => item.display.clone(),
                };
                self.input.original = replacement;
                self.input.cursor_byte = self.input.original.len();
                self.input.sync_display();
                // 更新面板阶段
                // ...
            }
            return;
        }
        KeyCode::Esc => {
            self.command_panel.clear();
            return;
        }
        _ => {} // 继续正常输入处理
    }
}
```

**Step 6: 渲染命令面板**

在 `App::render()` 方法中，在渲染输入区之后添加命令面板渲染:

```rust
// 渲染命令面板（在输入框上方）
self.command_panel.render(frame, input_area);
```

**Step 7: 输入 `:` 时激活命令面板**

在 `input.rs` 的 `apply_key()` Char(c) 分支中，输入后检查是否应该激活命令面板:

```rust
(KeyCode::Char(c), KeyModifiers::NONE) | (KeyCode::Char(c), KeyModifiers::SHIFT) => {
    // ... 现有输入逻辑 ...
    self.update_completion();

    // 命令面板：如果输入以 ':' 开头，自动弹出面板
    // 注意：面板的激活/过滤逻辑在 App 层处理
    // input.rs 通过回调或返回标志通知 App
}
```

实际上，更好的做法是在 `App::handle_key()` 中，每次输入字符后检查并更新 CommandPanel:

```rust
// App::handle_key 中 InputResult::Consumed 后
if self.input.original.starts_with(':') {
    self.command_panel.update_filter(&self.command_registry, &self.input.original);
} else {
    self.command_panel.clear();
}
```

**Step 8: 运行 cargo check**

Run: `cd rust && cargo check --package ai-brain-cli 2>&1 | tail -10`
Expected: 编译通过

**Step 9: 提交**

```bash
git add rust/crates/ai-brain-cli/src/tui/
git commit -m "feat: 集成命令面板到 TUI App，替换硬编码命令处理"
```

---

### Task 9: 替换 EvolutionCompleter 硬编码为动态注册表驱动

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/tui/completion.rs` — `EvolutionCompleter::default_commands()` 改为从 CommandRegistry 动态读取
- Modify: `rust/crates/ai-brain-cli/src/tui/app.rs` — 传入 CommandRegistry 到 completer

**Step 1: 修改 EvolutionCompleter**

添加 `from_registry()` 构造方法:

```rust
impl EvolutionCompleter {
    /// 从 CommandRegistry 构建补全器
    pub fn from_registry(registry: &CommandRegistry, template_names: Vec<String>, pattern_keywords: Vec<String>) -> Self {
        let builtin_commands: Vec<BuiltinCommand> = registry
            .all_commands()
            .iter()
            .map(|cmd| BuiltinCommand {
                name: cmd.name.to_string(),
                args_hint: if cmd.subcommands.is_empty() { None } else { Some("<子命令>".to_string()) },
                description: cmd.description.to_string(),
            })
            .collect();

        Self {
            builtin_commands,
            template_names,
            pattern_keywords,
        }
    }
}
```

**Step 2: 修改 App::new() 中的 completer 初始化**

```rust
// 从 CommandRegistry 和 orchestrator 构建补全器
let completer = {
    let (template_names, pattern_keywords) = orch.completion_data();
    EvolutionCompleter::from_registry(&command_registry, template_names, pattern_keywords)
};
```

**Step 3: 运行全量测试**

Run: `cd rust && cargo test --package ai-brain-cli -- --nocapture 2>&1 | tail -10`
Expected: 全部通过（现有的 completion 测试也应通过）

**Step 4: 提交**

```bash
git add rust/crates/ai-brain-cli/src/tui/completion.rs rust/crates/ai-brain-cli/src/tui/app.rs
git commit -m "feat: 补全器从 CommandRegistry 动态读取命令列表"
```

---

### Task 10: 集成测试 — 端到端命令面板交互

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/tui/command_panel.rs` — 添加更多测试

**Step 1: 写端到端测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::command;

    fn full_registry() -> CommandRegistry {
        command::build_full_registry()
    }

    #[test]
    fn test_full_registry_has_all_commands() {
        let reg = full_registry();
        assert!(reg.find_command("help").is_some());
        assert!(reg.find_command("status").is_some());
        assert!(reg.find_command("quit").is_some());
        assert!(reg.find_command("clear").is_some());
        assert!(reg.find_command("config").is_some());
        assert!(reg.find_command("plugin").is_some());
        assert!(reg.find_command("skill").is_some());
        assert!(reg.find_command("mcp").is_some());
        assert!(reg.find_command("memory").is_some());
        assert!(reg.find_command("evo").is_some());
    }

    #[test]
    fn test_panel_filter_plugin() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "pl");
        assert!(panel.visible);
        assert!(panel.items.iter().any(|i| i.command_name == "plugin"));
        assert!(!panel.items.iter().any(|i| i.command_name == "help"));
    }

    #[test]
    fn test_panel_subcommand_mode() {
        let mut panel = CommandPanel::new();
        // 输入 ":plugin " → 切换到子命令模式
        panel.update_filter(&full_registry(), "plugin ");
        assert!(panel.visible);
        assert!(panel.items.iter().any(|i| i.subcommand_name.as_deref() == Some("list")));
        assert!(panel.items.iter().any(|i| i.subcommand_name.as_deref() == Some("install")));
    }

    #[test]
    fn test_panel_subcommand_filter() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "plugin ins");
        assert!(panel.visible);
        assert!(panel.items.iter().any(|i| i.subcommand_name.as_deref() == Some("install")));
        assert!(!panel.items.iter().any(|i| i.subcommand_name.as_deref() == Some("list")));
    }

    #[test]
    fn test_panel_no_match() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "zzz");
        assert!(!panel.visible);
    }
}
```

**Step 2: 运行测试**

Run: `cd rust && cargo test --package ai-brain-cli tui::command_panel::tests -- --nocapture`
Expected: 全部通过

**Step 3: 运行 cargo clippy**

Run: `cd rust && cargo clippy --package ai-brain-cli --all-targets -- -D warnings 2>&1 | tail -10`
Expected: 无 warnings

**Step 4: 提交**

```bash
git add rust/crates/ai-brain-cli/src/
git commit -m "feat: 命令面板集成测试，验证端到端交互"
```

---

### Task 11: 全量回归测试 + 清理

**Files:**
- 全 workspace

**Step 1: 运行完整测试套件**

Run: `cd rust && cargo test --workspace --exclude brain-integration-tests 2>&1 | tail -20`
Expected: 全部通过，0 failed

**Step 2: 运行 cargo fmt**

Run: `cd rust && cargo fmt --all`
Expected: 无格式问题

**Step 3: 运行 cargo clippy**

Run: `cd rust && cargo clippy --workspace --all-targets --exclude brain-integration-tests -- -D warnings 2>&1 | tail -20`
Expected: 无 warnings

**Step 4: 清理废弃代码**

- 删除 `completion.rs` 中 `EvolutionCompleter::default_commands()` 方法（已被 `from_registry()` 替代）
- 确认 `input.rs` 中 `EvolutionCompleter` 的使用方式仍然正确
- 清理所有 `#[allow(dead_code)]` 标注

**Step 5: 最终提交**

```bash
git add rust/
git commit -m "chore: 命令系统回归测试 + 清理"
```
