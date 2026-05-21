# 斜杠命令系统设计

> 日期: 2026-05-21
> 状态: 已批准

## 目标

为 AI Brain TUI 添加 Claude Code 风格的命令交互系统，支持：
- 冒号前缀 (`:`) 命令
- 输入即时弹出下拉命令面板
- 实时字符过滤匹配
- 两级命令结构（`:` + 命令 + 子命令）
- 动态参数补全（从 Plugin/Skill/MCP 实时获取）
- 混合执行模式（简单命令同步，耗时操作异步）

## 方案选择

**选定方案 A：命令注册表 + 动态补全**

- 定义 `CommandRegistry`，所有命令统一注册
- 插件/Skill/MCP 命令可动态注册
- TUI 从注册表动态构建补全列表

淘汰方案：
- 方案 B（硬编码枚举）：无法动态注册插件命令
- 方案 C（外部进程）：过度工程

## 架构

### 核心类型

```rust
// command/registry.rs

/// 命令定义
struct Command {
    name: &'static str,
    description: &'static str,
    group: CommandGroup,
    subcommands: Vec<SubCommand>,
    handler: CommandHandler,
}

struct SubCommand {
    name: &'static str,
    description: &'static str,
    args: Vec<ArgSpec>,
}

struct ArgSpec {
    name: &'static str,
    required: bool,
    completer: Option<ArgCompleter>,
}

enum CommandGroup {
    BuiltIn,
    Plugin,
    Skill,
    Mcp,
    Memory,
    Evolver,
    Config,
}

enum CommandHandler {
    Sync(fn(&App, &[String]) -> CommandResult),
    Async(fn(&App, &[String]) -> BoxFuture<'static, CommandResult>),
}

struct CommandResult {
    output: String,
    success: bool,
}

/// 命令注册表
struct CommandRegistry {
    commands: Vec<Command>,
}
```

### 注册流程

```
App::new()
  → CommandRegistry::new()
  → register_builtin_commands()      // help/status/quit/clear
  → register_config_commands()       // config get/set/brain-params
  → register_plugin_commands(&orch)  // 从 PluginManager 动态注册
  → register_skill_commands(&catalog)// 从 SkillCatalog 动态注册
  → register_mcp_commands(&pool)     // 从 McpClientPool 注册
  → register_memory_commands()       // memory stats/recall/save/daily
  → register_evolver_commands()      // evo 系列
```

## TUI 下拉命令面板

### 状态机

```
Normal → (输入 ':') → CommandMode → (输入字符) → 实时过滤
                                          ↓ (Tab/Enter)
                                     确认选择
                                          ↓ (空格)
                                     SubCommandMode → 实时过滤子命令
                                          ↓ (Tab/Enter)
                                     确认子命令
                                          ↓ (空格后输入参数)
                                     ArgsMode
                                          ↓ (Enter)
                                     执行命令
                                          ↓ (Esc 任何时刻)
                                     Normal
```

### UI 布局

```
┌─────────────────────────────────────┐
│                                     │
│         (输出区域)                   │
│                                     │
├─────────────────────────────────────┤
│ :plugin ────────────────────────┐   │
│ ┌───────────────────────────────┤   │
│ │ ▸ :plugin install <path>     │   │ ← 下拉面板
│ │   :plugin uninstall <name>   │   │
│ │   :plugin list               │   │
│ ├───────────────────────────────┤   │
│ │   :help        帮助           │   │ ← 其他匹配项
│ │   :status      系统状态       │   │
│ └───────────────────────────────┘   │
│ Ready │ MainBrain │ 0 turns        │ ← 状态栏
└─────────────────────────────────────┘
```

### 核心组件

```rust
// tui/command_panel.rs

struct CommandPanel {
    visible: bool,
    items: Vec<CommandPanelItem>,
    selected: usize,
    query: String,
    phase: PanelPhase,
}

enum PanelPhase {
    Command,
    SubCommand { command_name: String },
    Args { command_name: String, subcommand_name: String },
}

impl CommandPanel {
    fn update_filter(&mut self, registry: &CommandRegistry, input: &str);
    fn render(&self, frame: &mut Frame, area: Rect);
    fn move_selection(&mut self, delta: i32);
    fn confirm(&mut self) -> Option<CommandPanelResult>;
}
```

## 完整命令清单

### 内置命令

| 命令 | 子命令 | 参数 | 模式 | 说明 |
|------|--------|------|------|------|
| `:help` | — | — | 同步 | 显示所有命令帮助 |
| `:status` | — | — | 同步 | 系统状态 |
| `:quit` | — | — | 同步 | 退出 |
| `:clear` | — | — | 同步 | 清空输出区 |

### 配置命令

| 命令 | 子命令 | 参数 | 模式 | 说明 |
|------|--------|------|------|------|
| `:config` | get | `<key>` | 同步 | 查看配置项 |
| | set | `<key> <value>` | 同步 | 修改配置项 |
| | brain-params | — | 同步 | 查看各脑参数 |

### 插件命令

| 命令 | 子命令 | 参数 | 模式 | 说明 |
|------|--------|------|------|------|
| `:plugin` | list | — | 同步 | 列出已安装插件 |
| | install | `<path>` | 异步 | 安装插件 |
| | uninstall | `<name>` | 异步 | 卸载插件 |
| | reload | — | 异步 | 重新加载插件 |

### Skill 命令

| 命令 | 子命令 | 参数 | 模式 | 说明 |
|------|--------|------|------|------|
| `:skill` | list | — | 同步 | 列出可用 skills |
| | run | `<name>` | 异步 | 执行指定 skill |
| | info | `<name>` | 同步 | 查看 skill 详情 |

### MCP 命令

| 命令 | 子命令 | 参数 | 模式 | 说明 |
|------|--------|------|------|------|
| `:mcp` | list | — | 同步 | 列出 MCP 服务器 |
| | status | `<server>` | 同步 | 查看服务器状态 |
| | reconnect | `<server>` | 异步 | 重连服务器 |

### 记忆命令

| 命令 | 子命令 | 参数 | 模式 | 说明 |
|------|--------|------|------|------|
| `:memory` | stats | — | 同步 | 记忆统计 |
| | recall | `<query>` | 异步 | 召回记忆 |
| | save | `<text>` | 异步 | 手动保存记忆 |
| | daily | `[date]` | 异步 | 查看每日摘要 |

### 进化脑命令

| 命令 | 子命令 | 参数 | 模式 | 说明 |
|------|--------|------|------|------|
| `:evo` | — | `<目标>` | 异步 | 启动进化任务 |
| | status | — | 同步 | 查看进化状态 |
| | approve | — | 异步 | 批准进化结果 |
| | reject | — | 异步 | 拒绝进化结果 |
| | diff | — | 同步 | 查看代码变更 |

## 参数动态补全

| 命令参数 | 补全来源 |
|----------|----------|
| `:config get/set <key>` | 当前配置 keys |
| `:plugin uninstall <name>` | PluginManager.list() |
| `:skill run/info <name>` | SkillCatalog 动态列表 |
| `:mcp status/reconnect <server>` | McpClientPool 服务器列表 |
| `:memory daily [date]` | 最近 7 天日期 |

## 执行模型

```rust
fn execute_command(app: &App, cmd: &ParsedCommand) {
    match cmd.handler {
        CommandHandler::Sync(f) => {
            let result = f(app, &cmd.args);
            app.output.push_line(result.output);
        }
        CommandHandler::Async(f) => {
            app.show_progress(format!("执行 :{} {}...", cmd.name, cmd.subcommand));
            tokio::spawn(async move {
                let result = f(app, &cmd.args).await;
                app.send_event(AppEvent::CommandComplete(result));
            });
        }
    }
}
```

## 文件改动范围

### 新建文件

| 文件 | 说明 |
|------|------|
| `ai-brain-cli/src/command/mod.rs` | 模块入口 |
| `ai-brain-cli/src/command/registry.rs` | CommandRegistry + Command/SubCommand 类型 |
| `ai-brain-cli/src/command/builtin.rs` | help/status/quit/clear 实现 |
| `ai-brain-cli/src/command/config_cmd.rs` | config get/set/brain-params 实现 |
| `ai-brain-cli/src/command/plugin_cmd.rs` | plugin list/install/uninstall/reload 实现 |
| `ai-brain-cli/src/command/skill_cmd.rs` | skill list/run/info 实现 |
| `ai-brain-cli/src/command/mcp_cmd.rs` | mcp list/status/reconnect 实现 |
| `ai-brain-cli/src/command/memory_cmd.rs` | memory stats/recall/save/daily 实现 |
| `ai-brain-cli/src/command/evolver_cmd.rs` | evo 系列 实现 |
| `ai-brain-cli/src/tui/command_panel.rs` | 下拉命令面板 UI 组件 |

### 修改文件

| 文件 | 说明 |
|------|------|
| `ai-brain-cli/src/tui/app.rs` | 替换 handle_builtin_command_sync 为 CommandRegistry 驱动 |
| `ai-brain-cli/src/tui/input.rs` | 检测 `:` 前缀切换到命令面板模式 |
| `ai-brain-cli/src/tui/completion.rs` | 替换硬编码 EvolutionCompleter 为动态补全 |
| `ai-brain-cli/src/main.rs` | 添加 `mod command` |
