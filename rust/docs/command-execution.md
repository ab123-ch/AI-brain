# 智脑命令执行后端配置

AI Brain 的通用 `bash` 工具支持 `auto`、`wsl`、`powershell` 和 `sh` 四种后端。
Windows 未配置或配置为 `auto` 时默认进入 WSL 的 `/bin/bash -lc`；macOS 和 Linux
默认继续使用宿主 `sh -lc`。

Web 服务、SQLite、文件工具、MCP 和 Tailscale 仍运行在宿主系统。只有模型调用通用
`bash` 工具时使用这里选择的命令后端。显式 `PowerShell` 工具始终执行 PowerShell，
不读取本配置。

## 配置格式

在用户或项目的 JSON 设置文件中加入：

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

- `backend` 可选值为 `auto`、`wsl`、`powershell`、`sh`，缺省为 `auto`。
- `wsl.distribution` 可选，缺省时使用 Windows 当前默认发行版。
- `wsl.user` 可选，缺省时使用所选发行版的默认用户。
- `wsl` 和 `powershell` 只能在 Windows 使用；非法配置会在模型执行前报错，不会回退。

只指定后端时可以使用更短的配置：

```json
{
  "commandExecution": {
    "backend": "powershell"
  }
}
```

保留 Git Bash/MSYS 或原有宿主 `sh` 行为：

```json
{
  "commandExecution": {
    "backend": "sh"
  }
}
```

## 配置来源和优先级

后读取的文件覆盖先读取的文件，嵌套对象按字段深合并：

1. 用户兼容配置 `%USERPROFILE%\.claw.json`；
2. 用户配置 `%USERPROFILE%\.claw\settings.json`；
3. 当前工作目录的 `.claw.json`；
4. 当前工作目录的 `.claw\settings.json`；
5. 当前工作目录的 `.claw\settings.local.json`。

`CLAW_CONFIG_HOME` 可以覆盖用户 `.claw` 目录的位置。在 macOS/Linux 上，用户目录写法
对应 `$HOME/.claw.json` 和 `$HOME/.claw/settings.json`。

项目设置由宿主进程读取，所以无需把配置文件放进 WSL。项目级设置适合让不同 Web 房间
使用不同后端；本地且不应提交的覆盖项放在 `.claw/settings.local.json`。

## 生效时机

- 新创建的 Web 成员持久任务读取一次设置，并把解析结果冻结到
  `collaboration-task-v6`；后续恢复不会重读已变化的文件。
- 修改设置后，新任务使用新值，已经创建、排队或运行中的 v6 任务保持原后端。
- 直接 TUI、`query`、`serve` 或 `web` 主脑在 Orchestrator 初始化时冻结配置；重新启动
  对应进程后使用新值。
- 委派 Agent 继承父运行的后端、工作目录、WSL 发行版和用户，不单独读取配置。

## Windows 路径与安全边界

WSL 后端接受 Windows 本地盘符绝对工作目录，例如
`C:\workspace\claw-code-parity`。UNC、`\\wsl.localhost\...` 和设备路径不受支持；请把
工作区放到 Windows 本地盘，或切换为 `powershell`/`sh`。

模型在 WSL Bash 中使用 POSIX 路径和命令语法；`read_file`、`write_file` 等文件工具仍
使用 Windows 宿主路径。WSL 不是沙箱，默认可以访问挂载的 Windows 盘和发行版文件系统。

## 排障

在 Windows PowerShell 中检查：

```powershell
wsl --status
wsl --list --verbose
wsl --exec /bin/bash -lc 'printf "wsl-ok\n"'
```

常见处理：

- 找不到 `wsl.exe`：安装 WSL，或把 `backend` 改为 `powershell`/`sh`。
- 找不到发行版或用户：修正 `wsl.distribution`/`wsl.user`，或删除字段使用默认值。
- 找不到 `/bin/bash`：在所选发行版安装 Bash，或选择另一个发行版。
- 命令语法错误：确认模型提示和工具输出中的 `executionBackend`、`commandSyntax`。

`auto` 只选择平台默认值，不代表失败后自动尝试其他后端。
