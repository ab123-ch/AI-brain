# Windows 默认 WSL 命令后端与可配置切换设计

## 背景

AI Brain Web 群会话中的实例通过 `CollaborationRuntime -> Orchestrator ->
RealToolExecutor -> tools -> runtime` 执行工具。每个成员运行已经冻结一个 Windows
宿主绝对工作目录，并通过 `ToolExecutionContext` 传给主脑和所有工作区工具。

当前命令执行有两条独立路径：

- `bash` 工具在所有平台固定启动 `sh -lc <command>`；原生 Windows 没有
  `sh.exe` 时直接失败，有 Git Bash 等环境时才可能碰巧可用。
- `PowerShell` 工具在 Windows 优先启动 `pwsh.exe`，找不到时使用
  `powershell.exe`。

因此当前系统不是“Windows 默认 PowerShell”，也没有统一的平台后端选择。模型同时
看到两个工具，自行选择；其中最常用的 `bash` 在 Windows 上没有可靠的默认实现。

## 目标

- 原生 Windows 上，默认使用 WSL 中的 Bash 执行 `bash` 工具命令。
- 用户可以通过现有分层 JSON 设置切换默认命令后端，并可指定 WSL 发行版和用户。
- 未配置时，macOS/Linux 继续使用宿主 `sh -lc`，不改变现有行为。
- Web 群成员、直接主脑和委派子代理使用同一套解析结果和启动实现。
- 一次已接纳的运行冻结命令后端；配置修改只影响之后启动的运行。
- 持久协作任务重启恢复后继续使用首次接纳时的后端。
- 保留显式 `PowerShell` 工具，调用它时始终执行 PowerShell，不受默认后端影响。
- WSL 缺失或配置错误时返回可操作错误，不静默切换到另一种解释器。

## 非目标

- 不把 `ai-brain.exe web`、SQLite、文件 API、Tailscale 或 MCP 服务迁入 WSL。
- 不改变 Hook、插件 Hook、MCP stdio、REPL 或 Tailscale 命令的解释器。
- 不自动安装、更新、启动发行版初始化流程或修改 Windows 的 WSL 默认发行版。
- 不新增 Web 配置页面，也不允许模型通过 `Config` 工具永久修改命令后端。
- 不把 WSL 当作安全边界，不宣称 Windows 宿主沙箱已在 WSL 内生效。
- 首版不让宿主文件工具自动接受 `/mnt/c/...` 或 `/home/...` 形式的 WSL 绝对路径。
- 不重命名现有 `bash` 工具，不迁移已有 `bash(...)` 权限规则。

## 已确认的产品语义

### 默认规则

配置中的 `auto` 是平台默认选择，不是失败降级：

| 宿主平台 | 未配置或 `auto` | 命令语法 |
|---|---|---|
| Windows | `wsl` | POSIX Bash |
| macOS/Linux | `sh` | POSIX `sh` |

Windows 的 `auto` 解析成 WSL 后，如果 `wsl.exe`、默认发行版或 `/bin/bash` 不可用，
对应的命令调用失败并提示安装 WSL 或修改配置。系统不得自动尝试 PowerShell 或宿主 `sh`，
因为解释器切换可能让同一条命令产生完全不同的行为。

### 可选后端

| 配置值 | 适用平台 | 实际启动方式 | 用途 |
|---|---|---|---|
| `auto` | 全部 | Windows -> `wsl`，其他 -> `sh` | 推荐默认值 |
| `wsl` | Windows | `wsl.exe ... /bin/bash -lc` | Windows 默认 POSIX 环境 |
| `powershell` | Windows | 优先 `pwsh.exe`，回退 `powershell.exe` | 显式切换默认命令语法 |
| `sh` | 全部 | 宿主 PATH 中的 `sh -lc` | Git Bash/MSYS 或兼容旧环境 |

首版不提供 `cmd` 和任意自定义可执行文件后端。Windows 原生命令优先使用
PowerShell；任意程序后端会扩大配置注入和兼容性测试范围，当前没有必要。

### 生效时机

- 新的直接主脑运行或 Web 成员运行在接纳时解析一次配置；委派子代理继承父运行的结果，
  不再次解析。
- 同一运行内的所有 `bash` 调用使用同一个已解析后端。
- 修改配置不要求重启 Web 服务，但不会改变已经排队后接纳、正在执行或恢复中的
  v6 持久任务。
- 配置修改后的下一次新运行使用新值。
- `PowerShell` 工具始终强制 PowerShell；它不读取 `commandExecution.backend`。

## 配置契约

### 配置格式

配置沿用 runtime 的 JSON 设置体系：

```json
{
  "commandExecution": {
    "backend": "auto",
    "wsl": {
      "distribution": "Ubuntu-24.04",
      "user": "brain"
    }
  }
}
```

字段语义：

- `backend`：可选，允许 `auto`、`wsl`、`powershell`、`sh`；缺省为 `auto`。
- `wsl.distribution`：可选；缺省时使用 Windows 当前默认 WSL 发行版。
- `wsl.user`：可选；缺省时使用所选发行版的默认用户。

`distribution` 和 `user` 去除首尾空白后不得为空，不允许 NUL。它们永远作为独立进程
参数传递，不拼接进 shell 命令。`wsl` 子表在其他后端下作为休眠配置保留，方便用户
切回 WSL，不参与当前启动。

不开放 WSL 内 shell 路径配置。首版固定 `/bin/bash -lc`，这样模型看到的语法稳定，
也避免用户配置一个不支持 `-lc` 的程序。如果所选发行版没有 `/bin/bash`，返回明确
错误；用户可以给该发行版安装 Bash、选择另一个发行版或改用其他后端。

### 配置来源和优先级

继续使用 `ConfigLoader` 的现有顺序，后读取者覆盖前者，嵌套对象做深合并：

1. 用户兼容配置 `%USERPROFILE%\.claw.json`；
2. 用户配置 `%USERPROFILE%\.claw\settings.json`；
3. 房间工作目录下 `.claw.json`；
4. 房间工作目录下 `.claw\settings.json`；
5. 房间工作目录下 `.claw\settings.local.json`。

`CLAW_CONFIG_HOME` 仍然具有最高的目录定位优先级。为保证纯 PowerShell 启动环境可用，
Windows 的用户配置目录解析顺序改为：

1. `CLAW_CONFIG_HOME`；
2. `HOME`；
3. `USERPROFILE`；
4. `HOMEDRIVE` + `HOMEPATH`；
5. 最后才使用相对 `.claw`。

项目配置由宿主 Rust 进程读取，不依赖 WSL 能否读取 JSON 文件。由于房间已经冻结宿主
工作目录，不同房间可以使用不同项目级后端，不修改进程全局 cwd。

### 配置示例

强制使用默认 WSL 发行版：

```json
{
  "commandExecution": {
    "backend": "wsl"
  }
}
```

指定发行版和用户：

```json
{
  "commandExecution": {
    "backend": "wsl",
    "wsl": {
      "distribution": "Ubuntu-24.04",
      "user": "brain"
    }
  }
}
```

切换为 PowerShell：

```json
{
  "commandExecution": {
    "backend": "powershell"
  }
}
```

保留原来的 Git Bash/宿主 `sh` 行为：

```json
{
  "commandExecution": {
    "backend": "sh"
  }
}
```

### 校验和权限

- `commandExecution` 不是对象、字段类型错误或 backend 不在枚举中时，运行在模型调用前
  失败，错误必须指出 `merged settings.commandExecution...`。
- `wsl` 或 `powershell` 配置在非 Windows 宿主上直接拒绝，不做平台猜测。
- 配置错误不得回退到默认值。
- 初版不把 `commandExecution.*` 加入模型可调用的 `Config` 工具白名单。用户通过文件、
  管理脚本或未来受权限保护的 UI 修改它。

## 总体架构

```text
用户/项目 .claw 设置
        |
        v
ConfigLoader + CommandExecutionConfig::resolve(host_os, cwd)
        |
        v
ResolvedCommandExecution（本轮不可变）
        |
        +--> Collaboration Task resolved_config（重启恢复）
        |
        +--> ToolExecutionContext
                |
                +--> MainBrain 环境提示词
                |
                +--> RealToolExecutor
                |       |
                |       v
                |    tools -> runtime launcher
                |
                +--> Agent -> SubagentToolExecutor（原样继承）
```

后端只在一处解析，提示词、执行器和持久恢复使用同一份结果。禁止由提示词按 OS 猜测一种
后端、执行器在调用时再独立解析另一种后端。

## 类型与 crate 边界

### runtime 配置类型

在 `runtime::config` 增加原始配置：

```rust
enum CommandBackendPreference {
    Auto,
    Wsl,
    Powershell,
    Sh,
}

struct WslCommandConfig {
    distribution: Option<String>,
    user: Option<String>,
}

struct CommandExecutionConfig {
    backend: CommandBackendPreference,
    wsl: WslCommandConfig,
}
```

解析函数接收可注入的宿主平台标识，不能把全部测试写成 `cfg!(windows)` 分支，从而让
Linux CI 也能验证 Windows 默认选择和非法组合。

### `brain-core` 冻结执行描述

`ToolExecutionContext` 增加平台中立、可序列化的描述：

```rust
enum ResolvedCommandBackend {
    Wsl,
    Powershell,
    Sh,
}

enum CommandSyntax {
    Posix,
    Powershell,
}

struct ResolvedCommandExecution {
    backend: ResolvedCommandBackend,
    syntax: CommandSyntax,
    host_os: String,
    wsl_distribution: Option<String>,
    wsl_user: Option<String>,
}

struct ToolExecutionContext {
    working_directory: PathBuf,
    command_execution: ResolvedCommandExecution,
}
```

这里不保存发现到的 `wsl.exe` 或 `pwsh.exe` 绝对路径。持久任务冻结的是解释器语义，
实际系统可执行文件在调用时安全发现，允许系统正常升级。`host_os` 用于阻止把 Windows
冻结任务复制到另一平台后静默改写语义。

平台进程构造仍归 `runtime` 所有。为了保持当前依赖方向，`runtime` 不依赖
`brain-core`：

- `brain-core` 定义上面的持久化/提示词中立描述；
- `runtime` 定义只供 launcher 使用的 `RuntimeCommandSpec` 和平台进程类型；
- `tools` 增加对 `brain-core` 的直接依赖，在唯一的执行桥接函数中把
  `ResolvedCommandExecution` 穷举转换成 `RuntimeCommandSpec`；
- `ai-brain-cli` 把 runtime 配置解析结果构造成 `brain-core` 描述，之后不再重新做后端选择。

这不是第二次解析：转换不得读取 OS、配置文件、环境变量或 PATH。两个内部类型字段一一
对应，并用穷举匹配和往返测试防止新增 backend 时漏传。这样既不产生
`runtime -> brain-core` 的反向依赖，也不把具体进程 launcher 下沉到领域上下文。

## 运行与恢复

### Web 群成员

`CollaborationRuntime::prepare_task` 调整顺序：

1. 根据 `task_run_id` 查询已有持久任务。
2. 已有 v6 任务从 `resolved_config.command_execution` 反序列化并严格校验。
3. 已有 v3-v5 任务使用兼容描述 `legacy host sh`，不应用新配置。
4. 没有任务时，根据 Claim 冻结工作目录加载分层设置并解析后端。
5. 构建包含工作目录和后端的 `ToolExecutionContext`。
6. 新任务把同一描述写入 `resolved_config` 后再执行。

新的任务版本统一为 `collaboration-task-v6`。v6 的 `member_handoff` 可空，但字段必须持久
化（没有 handoff 时为 `null`）。v6 先执行 v4/v5 共用的工作目录、回复引用和上下文校验，
再对持久值和 Claim 做成对匹配：两边都为空时通过，两边都有值时执行现有 v5 handoff
校验并比较相等，只有一边有值时视为任务损坏。这样不需要为同一功能再拆 v6/v7。

v6 至少持久化：

```json
{
  "execution_working_directory": "C:\\workspace\\project",
  "member_handoff": null,
  "command_execution": {
    "backend": "wsl",
    "syntax": "posix",
    "host_os": "windows",
    "wsl_distribution": "Ubuntu-24.04",
    "wsl_user": "brain"
  }
}
```

任务恢复时不重新读取当前设置。字段缺失、类型/空值错误、`syntax` 与 backend 不一致、
非 WSL backend 带有 WSL 专属字段，或 `host_os` 与当前宿主不一致，均视为持久任务损坏，
在模型调用前失败。合法的持久 distribution/user 本身就是恢复时的权威值；本设计不声称
能识别对数据库中一个合法值的恶意替换，数据库完整性保护属于单独的安全边界。

v3-v5 是功能上线前创建的历史任务。它们在 Windows 恢复时继续使用宿主 `sh -lc`，
在 macOS/Linux 也继续使用宿主 `sh -lc`。这样不会让一个已经部分执行过的任务在重启
后突然换解释器。用户重新发送或重试形成的新任务进入 v6，并采用新的 Windows WSL
默认值。

### 直接主脑

直接 TUI/Web 单实例查询在创建本轮 `ToolExecutionContext` 时，从当前请求工作目录解析
设置。构造失败必须返回配置错误，不创建只带默认 cwd 的上下文。`MainBrain::new` 的旧
便捷入口保留给测试/兼容调用，生产装配使用显式上下文入口。

### 委派子代理

普通工具生产入口以及 `execute_agent_tool_with_completion_in_directory` 的 Agent 专用异步
入口都增加接收父 `ToolExecutionContext` 的变体。子代理继承：

- 同一个宿主工作目录；
- 同一个已解析命令后端；
- 同一个 WSL 发行版和用户选择。

`SubagentToolExecutor` 不重新读取设置。子代理系统提示词显示继承的后端，避免父脑使用
WSL、子代理却因中途配置变化使用 PowerShell。

## 模型可见契约

保留工具名 `bash`，避免破坏：

- 现有模型工具调用历史；
- `bash(...)` 权限规则；
- 子代理工具白名单；
- Hook 中的工具名匹配；
- 现有测试和插件集成。

工具描述改为平台中立表述：

```text
Execute a command in the current workspace using the configured command backend.
On native Windows the default backend is WSL Bash.
```

`MainBrain` 和子代理的运行环境信息增加：

```text
- 宿主操作系统: Windows
- 宿主工作目录: C:\workspace\project
- 默认命令后端: WSL Bash（发行版: Ubuntu-24.04，用户: 默认）
- 命令语法: POSIX shell
- 路径约定: shell 与文件工具之间优先使用相对工作区路径
```

PowerShell 后端显示 `PowerShell` 语法；宿主 sh 显示 `POSIX shell`。不把完整 PATH、环境
变量或凭据注入提示词。

## 启动器设计

### 统一 launcher

`runtime::bash` 把当前重复的同步/异步进程构造收敛为纯数据 launcher：

```rust
struct CommandLauncher {
    backend: RuntimeCommandBackend,
    program: OsString,
    args: Vec<OsString>,
    current_dir: PathBuf,
    env: Vec<(OsString, OsString)>,
}
```

前台、后台和 Tokio 前台执行都消费同一 launcher。测试直接断言 program/args/current_dir，
不需要在非 Windows CI 上伪装真实 WSL。

### 校验与发现时机

运行接纳阶段只做不启动进程的静态工作：加载/合并设置、解析 `auto`、校验 backend 与宿主
组合、校验 WSL 工作目录可表示性，以及恢复任务的 `host_os`。配置或路径错误在模型调用前
失败。

可执行文件发现以及 WSL 发行版、用户、`/bin/bash` 和挂载状态在真正调用 `bash` 时检查。
接纳阶段不额外执行 `wsl.exe --status` 或探测命令：没有调用命令的纯文本回复不应被本机
WSL 状态阻塞，也避免接纳与实际调用之间的 TOCTOU。动态失败只让该工具调用报错，并带上
已冻结 backend 的修复提示；绝不重新解析配置或尝试另一后端。

### WSL 后端

Windows 下构造：

```text
<WindowsSystemDirectory>\wsl.exe
  [--distribution <distribution>]
  [--user <user>]
  --cd <host-working-directory>
  --exec /bin/bash -lc <command>
```

实现要求：

- 生产代码通过 Windows 系统目录 API 定位绝对路径 `System32\wsl.exe`；文件不存在时直接
  报 WSL 未安装，不从当前工作目录或 PATH 搜索同名程序。
- launcher builder 接收可注入的 executable locator，测试替身通过依赖注入提供，不改变
  生产发现策略。这样工作区中的伪造 `wsl.exe` 不会被当成系统组件执行。
- `distribution`、`user`、cwd 和 command 全部作为独立 `OsString` 参数传给 `Command`，
  不手工拼接或 shell 转义。
- 同时给 Windows `wsl.exe` 进程设置 `current_dir(host-working-directory)`；权威的 Linux
  起始目录仍由 `--cd` 指定。
- 不设置或修改 `WSLENV`，不自动把宿主 API Key、代理或其他环境变量转发进发行版。
- 使用所选发行版默认的 Linux 环境和登录配置。需要给 WSL 命令使用的变量由用户在
  发行版内配置，或显式配置 Windows/WSL 的环境转发。
- 不调用 `wsl --install`、`wsl --update`、`wsl --set-default` 或 `wsl --terminate`。

微软 Learn 已确认 `--distribution` 和 `--user` 的公开行为。`--cd` 的 Windows 绝对路径
兼容性与参数顺序必须在目标 Windows 主机执行 `wsl.exe --help` 和端到端验收后才能标记
完成；不能只凭跨平台参数构造测试发布。

### PowerShell 后端

复用显式 PowerShell 工具的发现顺序：

1. `pwsh.exe`；
2. `powershell.exe`。

启动参数为：

```text
<resolved-powershell> -NoProfile -NonInteractive -Command <command>
```

工作目录是宿主冻结目录。找不到两个程序时失败，不回退 WSL 或 sh。显式
`PowerShell` 工具继续使用同一底层构造器，但不读取默认 backend。

### 宿主 sh 后端

启动方式保持：

```text
sh -lc <command>
```

在 Windows 中由 PATH 解析 `sh.exe`，主要面向 Git Bash/MSYS 用户。找不到时给出配置
错误，不回退 PowerShell。

## 工作目录与路径

### 权威路径

房间、事件、Task 和文件工具继续保存 Windows 宿主规范绝对路径。不得把
`C:\workspace\project` 改写为 `/mnt/c/workspace/project` 后存入数据库，因为：

- SQLite 和工作区快照运行在 Windows 进程；
- read/write/edit/glob/grep 使用 Windows 文件 API；
- 同一目录可能由非 WSL 后端执行；
- WSL 的 automount 根可以由用户配置，不保证永远是 `/mnt`。

### 传给 WSL 的路径

Windows `canonicalize` 可能产生 `\\?\C:\...`。传给 `wsl.exe --cd` 前使用专用纯函数
转换为普通 drive 路径 `C:\...`，但数据库中的权威规范路径不变。

首版 WSL 后端支持盘符绝对路径。以下路径在运行接纳阶段拒绝并给出切换后端建议：

- 网络 UNC，例如 `\\server\share\project`；
- WSL UNC，例如 `\\wsl.localhost\Ubuntu\home\...`；
- 无法无损转换的 device namespace 路径。

这与 Windows 操作手册推荐把仓库放在 `C:\workspace\...` 一致，也避免假设网络共享或
另一发行版内部路径一定能被当前发行版挂载。后续可在真实需求出现时单独扩展。

路径含空格、中文或其他 Unicode 字符必须可用。由于 cwd 是独立 argv，不允许通过给
整个命令字符串加引号来实现。

### shell 与文件工具的边界

`--cd` 让相对路径在两侧指向同一个项目：

- WSL：`./src/main.rs`；
- Windows 文件工具：`src\main.rs` 或 `src/main.rs`。

模型跨工具传递路径时应优先使用相对路径。首版不自动把以下绝对路径互转：

- WSL 输出的 `/mnt/c/workspace/project/file.rs`；
- Windows 的 `C:\workspace\project\file.rs`；
- WSL 发行版内部 `/home/...`。

WSL 命令写入工作区内的文件仍会被 Windows 工作区快照检测。写到 `/home` 等发行版
内部位置的文件不属于房间工作区，不会出现在“本轮修改文件”中，也不能直接交给宿主
文件工具。

## 超时、后台与进程生命周期

- 保留当前默认 120 秒超时和调用参数覆盖。
- 前台 Tokio 子进程启用 `kill_on_drop(true)`；超时后终止 launcher 并等待回收。
- 不允许为终止单条命令调用 `wsl --terminate`，因为那会杀死同发行版中的其他任务。
- 原生 Windows 验收必须证明超时后命令不会延迟创建 marker 文件。若只杀死
  `wsl.exe` 仍遗留 Linux 进程，发布前必须增加单命令进程组清理，而不能把该行为记录
  成已知限制后上线。
- 后台模式继续返回 launcher 的 Windows PID，并保持 stdio 断开。返回字段明确命名为
  launcher PID，不把它冒充 Linux PID。
- 不在本次设计中增加后台任务查询/终止 API。

## 输出与可观察性

`BashCommandOutput` 增加向后兼容字段：

```json
{
  "executionBackend": "wsl",
  "commandSyntax": "posix",
  "hostWorkingDirectory": "C:\\workspace\\project",
  "wslDistribution": "Ubuntu-24.04",
  "wslUser": "brain",
  "backgroundTaskIdKind": "windows-launcher-pid"
}
```

非 WSL 字段为 `null`，前台调用的 PID kind 为 `null`。现有 stdout、stderr、timeout、
exit-code 和 sandbox 字段保持不变。

每次成员运行开始记录安全元数据：room/run/member、resolved backend、发行版和是否使用
默认用户。不得新增完整命令、环境变量或凭据日志；已有工具 trace 对命令正文的处理不在
本设计中扩大。

## 错误与降级策略

| 场景 | 发现时机 | 结果 | 是否自动降级 |
|---|---|---|---|
| backend 配置值非法 | 运行接纳 | 模型调用前返回配置路径和允许值 | 否 |
| WSL backend 使用不支持的宿主/路径 | 运行接纳 | 返回宿主和工作目录错误 | 否 |
| Windows 找不到 `wsl.exe` | `bash` 调用 | 提示安装 WSL 或配置 `powershell`/`sh` | 否 |
| 发行版、用户、Bash 或 `--cd` 初始化失败 | `bash` 调用 | 保留 WSL 原始 stderr、退出码和冻结上下文，并附统一排障命令 | 否 |
| 命令自身非零退出 | `bash` 调用 | 保留 stdout/stderr，标记 `exit_code:N` | 不适用 |
| 命令超时 | `bash` 调用 | `interrupted=true`，回收进程 | 不适用 |

`auto` 只负责在配置解析阶段选择平台默认值，不表示“依次尝试多个后端”。错误分类优先
依据 spawn error、退出状态和结构化上下文。WSL 的初始化错误可能使用本地化 stderr，且
与用户命令非零退出共享进程状态通道；没有稳定机器可读信号时不再细分错误类型，不用宽泛
字符串匹配把普通 Bash 退出误判为 WSL 安装故障。统一提示可包含当前 distribution/user、
宿主 cwd、`wsl --status`、`wsl --list --verbose` 和切换 backend 的配置位置，但不得覆盖原始
stdout/stderr。

## 权限、沙箱与安全

- 工具授权仍以 `bash` 为名执行现有 permission policy、PreToolUse/PostToolUse Hook 和
  审批规则；切换 backend 不提升权限。
- WSL 参数全部通过 argv 传递，配置值不得拼进 command 字符串。
- WSL 发行版及其 root 用户不是沙箱。选择 `wsl.user = root` 是用户配置行为，文档必须
  明确其风险。
- Windows 宿主无法使用当前 Linux `unshare` launcher。WSL 后端的 `sandboxStatus` 必须
  显示 `supported=false`、`active=false`，fallback reason 明确为宿主沙箱未应用到 WSL；
  不得因为子进程运行 Linux 就显示已启用。
- 未真正启用 namespace/filesystem sandbox 时，不为 WSL 重写 Linux `HOME`，也不声称
  `.sandbox-home` 限制了发行版访问。
- WSL 默认可访问挂载的 Windows 盘和发行版文件系统。现有工具权限负责是否允许调用
  命令，但首版没有 WSL 内文件系统隔离。
- 不自动转发宿主 secrets。用户主动配置 `WSLENV` 属于外部运行环境，不由 AI Brain
  持久化或展示。

## 兼容与迁移

### 保持兼容

- macOS/Linux 未配置时仍是 `sh -lc`。
- 工具名、输入 schema、权限名和 Hook 事件中的 `bash` 不变。
- 显式 `PowerShell` 工具行为不变。
- `BashCommandOutput` 只增加可选字段，旧客户端可以忽略。
- 房间工作目录、事件表和文件变更表不需要数据库迁移。
- v3-v5 持久任务按旧宿主 sh 语义恢复。

### 有意改变

- Windows 新运行的 `bash` 从“PATH 中碰巧存在的 sh”改为 WSL Bash。
- WSL 不可用时从不确定的 `sh not found` 变为明确配置/安装错误。
- backend 在一次运行中冻结，修改配置不会改变进行中的多步工具循环。
- 新协作任务写入 v6 resolved config。

### 回滚

部署后如果目标主机暂时不能使用 WSL，用户设置：

```json
{
  "commandExecution": {
    "backend": "powershell"
  }
}
```

即可让之后的新运行使用 PowerShell，无需回滚二进制。已经冻结为 WSL 的 v6 任务不会
被设置重写；用户应取消后重新提交，使系统创建新任务。

## 实现范围

### `runtime`

- `config.rs`：增加 commandExecution 类型、解析、默认解析、Windows home fallback。
- `bash.rs`：增加统一 launcher、WSL/PowerShell/sh builder、路径规范化和输出元数据。
- `lib.rs`：导出配置与执行类型。
- 保留旧 `execute_bash[_in_dir]` 兼容入口；新增接收冻结描述的入口供生产链使用。

### `brain-core` / `brain-main`

- `brain-core` 定义中立冻结描述，`ToolExecutionContext` 持有该描述。
- 环境提示词显示 backend、syntax 和路径边界。
- 测试构造器可显式注入 backend，避免依赖测试机安装的软件。

### `tools`

- 增加对 `brain-core` 的直接依赖和接收完整上下文的生产执行入口。
- `bash` 在单一桥接点把冻结描述穷举转换为 runtime spec；不读取设置或 OS。
- Agent 启动和 `SubagentToolExecutor` 原样继承描述。
- `PowerShell` 显式工具继续强制 PowerShell，共用底层 builder。
- 更新通用工具描述和输出序列化。

### `ai-brain-cli`

- 直接主脑装配时解析工作目录对应配置。
- Web `prepare_task` 按“先恢复/创建任务，再构造上下文”的顺序冻结 backend。
- 新任务写 v6；更新所有任务版本白名单，并校验 command descriptor 和 host OS。
- `GroupMessageToolExecutor` 继续原样透传整个上下文。
- 运行 trace 记录非敏感 backend 元数据。

### 文档

- Windows 操作手册增加 WSL 安装、发行版检查、配置示例和故障排查。
- README 说明 Web 服务仍原生运行，只有模型默认命令进入 WSL。
- 配置文档列出来源、优先级和生效时机。

## 测试设计

### 配置单元测试

- 注入 Windows 平台时，缺省和 `auto` 都解析为 `wsl`。
- 注入 macOS/Linux 时，缺省和 `auto` 解析为 `sh`。
- `wsl`、`powershell`、`sh` 显式值正确解析。
- 非法 backend、错误字段类型、空 distribution/user 返回精确路径错误。
- 用户、项目、本地配置深合并和覆盖正确。
- `CLAW_CONFIG_HOME`、`HOME`、`USERPROFILE`、`HOMEDRIVE/HOMEPATH` 优先级正确。

### launcher 纯函数测试

- WSL 默认 argv 的顺序和单参数 command 正确。
- 生产 locator 只接受 Windows 系统目录中的 `wsl.exe`，测试 locator 可注入替身。
- distribution/user 只作为独立 argv 出现。
- 含空格、中文、引号和 shell 元字符的 cwd/command 不被 Rust 层二次拼接。
- `\\?\C:\...` 只在 launcher 参数中规范为 `C:\...`。
- UNC、WSL UNC 和不支持的 device path 在 spawn 前拒绝。
- PowerShell 和 sh builder 保持既有参数与 cwd。
- `auto` 解析完成后 launcher 不再包含 auto 或 fallback 列表。

### runtime 行为测试

使用可执行测试替身覆盖：

- stdout/stderr 和零/非零退出；
- 默认/显式超时；
- 前台和后台 cwd；
- backend 输出元数据；
- 找不到程序时不尝试第二后端；
- sandbox unsupported 状态不虚报 active；
- 同一冻结上下文在配置文件变化后仍使用原 backend。

### 主脑与子代理测试

- 环境提示词准确显示 WSL、PowerShell、sh 及对应语法。
- `RealToolExecutor::execute_with_context` 使用传入 backend。
- GroupMessage wrapper 不丢失 backend。
- Agent/SubagentToolExecutor 继承父 backend，配置文件中途改变不影响子代理。
- 显式 PowerShell 工具在默认 WSL 上下文中仍使用 PowerShell。

### 持久任务测试

- 新任务统一写 v6 和完整 command_execution。
- v6 重启恢复不重新读取已变化的设置。
- backend/syntax 不变量、host_os、空 distribution/user 或非 WSL backend 的 WSL 字段无效时
  拒绝恢复；合法持久值作为冻结配置恢复。
- v6 有/无 member_handoff 分别保持 v5/v4 上下文校验；单边存在时拒绝恢复。
- v3-v5 恢复使用 legacy host sh。
- 新配置只影响之后创建的 task_run。

### 原生 Windows WSL 验收

以下验证必须在最终 Windows 主机或带 WSL 的自托管 Windows runner 执行，不能由参数
单元测试代替：

1. `wsl.exe --help` 确认目标版本支持 `--cd`、`--distribution`、`--user`、`--exec`。
2. 未配置时，`bash` 输出 backend=wsl，`pwd` 对应房间宿主目录。
3. 在含空格和中文的 `C:\workspace\...` 目录创建相对 marker，Windows 文件 API 可见。
4. 指定发行版和用户后，`cat /etc/os-release`、`whoami` 与配置一致。
5. WSL 不存在、无发行版、发行版错误、用户错误和 Bash 缺失均返回预期错误且不降级。
6. `powershell` 配置执行 PowerShell 语法，显式 PowerShell 工具仍正常。
7. `sh` 配置在安装 Git Bash 的主机保持旧语义。
8. 前台非零退出、stdout/stderr、120 秒默认值和短超时行为正确。
9. 超时命令不会在返回后延迟写 marker，不遗留 Linux 子进程。
10. 后台命令在冻结 cwd 写入 marker，返回值明确是 Windows launcher PID。
11. 两个房间并发使用不同盘符目录，不串 cwd、不修改进程全局 cwd。
12. 通过真实 WebSocket 群任务触发一次 `bash`，任务 v6、trace 和文件变更归属一致。

### CI 与回归

- 现有 Linux CI 运行配置、launcher、提示词、持久任务和通用 runtime 测试。
- 增加 `windows-latest` 的 MSVC 编译和不依赖真实发行版的 Windows 单元测试。
- 真实 WSL 测试放在具备 WSL 的自托管 Windows runner；如果暂时没有，该项必须保留为
  发布前人工 gate，不能标记为自动覆盖。
- 运行 `cargo fmt --check`、相关 crate 测试、严格 Clippy 和 `git diff --check`。

## 实施顺序

1. 增加配置类型、平台注入解析和 Windows home fallback。
2. 提取统一 launcher，完成 WSL/PowerShell/sh 构造与纯函数测试。
3. 扩展 `ToolExecutionContext`、环境提示词和 RealToolExecutor 传递链。
4. 让 Agent/SubagentToolExecutor 继承冻结描述。
5. 升级协作任务到 v6 并完成 v3-v5 兼容恢复。
6. 增加输出/trace 元数据和错误映射。
7. 更新 Windows 文档并运行跨平台回归。
8. 在原生 Windows 完成 WSL 验收后再启用默认行为发布。

## 验收标准

- 原生 Windows、无 backend 配置的新运行实际由 WSL Bash 执行。
- 用户能用 JSON 设置切换到指定 WSL 发行版/用户、PowerShell 或宿主 sh。
- WSL 故障绝不静默执行另一种解释器。
- 提示词、工具执行、子代理和持久恢复看到同一个冻结 backend。
- Windows 宿主目录中的相对读写与文件工具、变更快照一致。
- 旧任务、非 Windows 默认、权限规则和显式 PowerShell 工具保持兼容。
- 原生 Windows 的 `--cd`、Unicode cwd、timeout 清理和并发房间验证全部通过。

## 参考

- Microsoft Learn: `Basic commands for WSL`，用于确认发行版和用户选择语义：
  <https://learn.microsoft.com/en-us/windows/wsl/basic-commands>
- 目标 Windows 主机安装版本的 `wsl.exe --help` 是 `--cd`/`--exec` 最终验收依据。
