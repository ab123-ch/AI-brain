# 智脑 Windows 主机远程访问操作手册

## 目标拓扑

```text
手机（Tailscale + 浏览器）
        |
        | Tailnet 私有 HTTPS
        v
Windows 主机上的 Tailscale Serve（智脑启动时自动恢复）
        |
        | http://127.0.0.1:8080
        v
ai-brain.exe web
```

智脑、模型配置、工具执行和文件读写都发生在 Windows 主机。手机只是发送指令和查看结果，不是远程桌面。

## 当前状态

- 已实现普通 `ai-brain web` 启动时自动检查/启动 Tailscale 并恢复 Serve。
- Web 服务只绑定 Windows 回环地址 `127.0.0.1`。
- Tailscale Serve 提供 Tailnet 内的 HTTPS 地址。
- 本机浏览器可以继续直接访问；远程 Web 请求必须包含 Tailscale 用户身份头。
- WebSocket 会校验 HTTPS Origin，拒绝跨站控制。
- 首次成功连接后，固定 MagicDNS URL 会写入 `~/.ai-brain/config.toml` 的 `[remote_access]`。
- 已为常见 Windows 安装目录和 `tailscale.exe` 添加发现逻辑及单元测试。
- 尚未在真实 Windows 主机上完成编译、Tailscale 服务和手机端到端验证。

## 一、准备 Windows 主机

最低环境：

- Windows 10/11，或 Windows Server 2016 及以上。
- Git for Windows。
- Rust stable MSVC 工具链。
- Visual Studio Build Tools 2022，并安装“使用 C++ 的桌面开发”。
- Tailscale Windows 客户端。
- 可用的智脑模型/API 配置。

保持主机在线：

- Windows 主机接通电源，并把“接通电源后使设备进入睡眠”设置为“从不”；显示器可以正常熄屏。
- 如果是笔记本且需要合盖运行，还要把接通电源时的合盖操作设为“不采取任何操作”，并保证散热。
- 远程使用时可以按 `Win+L` 锁屏，但不要注销 Windows 用户。注销或重启会结束当前 PowerShell 中的 `ai-brain.exe`。
- Windows 重启后，需要重新登录并执行远程启动命令；本手册暂不把尚未实测的计划任务或 Windows 服务方案加入正式流程。

建议把仓库放在 Windows 原生目录，例如：

```text
C:\workspace\claw-code-parity
```

不要同时在 Windows 和 WSL 2 中各运行一套 Tailscale。此方案以 Windows 主机上的 Tailscale 服务为准。

## 二、取得最新代码

如果代码已经推送到 Git 远端，在 PowerShell 中执行：

```powershell
git clone <repo-url> C:\workspace\claw-code-parity
cd C:\workspace\claw-code-parity
git switch <branch>
git pull --ff-only
cd rust
```

如果仓库已经存在：

```powershell
cd C:\workspace\claw-code-parity
git status
git branch --show-current
git pull --ff-only
cd rust
```

确认以下文件存在：

```powershell
Test-Path .\crates\ai-brain-cli\src\remote_access.rs
Select-String -Path .\crates\ai-brain-cli\src\main.rs -Pattern 'Commands::Remote'
```

如果当前改动尚未提交或推送，需要先通过 Git 提交/补丁或可靠的文件同步方式把这份工作区带到 Windows，不能只在 Windows 上对旧分支运行测试。

## 三、安装并登录 Tailscale

1. 在 Windows 主机安装官方客户端：<https://tailscale.com/download/windows>
2. 从系统托盘打开 Tailscale，登录个人账号。
3. 在手机安装 Tailscale，并登录同一个账号/Tailnet。
4. 建议在 Windows Tailscale 客户端中启用 unattended mode，让 Tailscale 在登录界面仍保持在线。它只负责 Tailscale，不会让交互式启动的 `ai-brain.exe` 跨注销或重启继续运行。

在 PowerShell 中检查：

```powershell
Get-Service Tailscale
Get-Command tailscale -ErrorAction SilentlyContinue
& "$env:ProgramFiles\Tailscale\tailscale.exe" status
```

预期结果：

- `Tailscale` 服务状态为 `Running`。
- `status` 能看到 Windows 主机和手机。
- 即使 `tailscale.exe` 不在 PATH，智脑也会检查常见的 Program Files 安装目录。

本文后续用 `tailscale` 作为命令简写。如果 `Get-Command tailscale` 找不到它，请改用完整路径，例如：

```powershell
& "$env:ProgramFiles\Tailscale\tailscale.exe" serve status
```

## 四、安装 Rust 并执行 Windows 原生测试

从 <https://rustup.rs> 安装 Rust。选择 MSVC 工具链，然后重新打开 PowerShell：

```powershell
rustup default stable-x86_64-pc-windows-msvc
rustup update
rustc -Vv
cargo -V
```

在仓库的 `rust` 目录运行：

```powershell
cargo fmt -p ai-brain-cli -- --check
cargo test -p ai-brain-cli --lib remote
cargo test -p ai-brain-cli --lib -- --skip test_orchestrator_query
cargo build --release -p ai-brain-cli --bin ai-brain
.\target\release\ai-brain.exe remote --help
```

预期结果：

- 所有命令退出码为 0。
- 远程相关测试包含 Windows `tailscale.exe` 候选路径测试。
- 生成 `target\release\ai-brain.exe`。

`test_orchestrator_query` 会访问外部模型，先从确定性测试中跳过；完成模型配置后再单独验证。

## 五、启动智脑（自动恢复远程入口）

先确保普通智脑模式所需的模型/API 配置已经可用。然后在工作区根目录对应的 `rust` 目录执行正常 Web 启动命令：

```powershell
.\target\release\ai-brain.exe web
```

智脑会自动尝试启动已安装的 Tailscale Windows 服务、检查登录状态，并恢复到 `127.0.0.1:8080` 的私有 Serve 代理。首次启用 Tailscale Serve 时，终端可能显示一个 HTTPS 授权地址，按提示在浏览器中批准一次。以后继续使用同一条 `web` 命令，不再需要手工执行 Tailscale 命令。

成功后终端会显示类似地址：

```text
https://windows-host.example.ts.net
```

保持这个 PowerShell/智脑进程运行。锁屏不会影响它，但注销或重启后需要重新执行该命令。端口反向代理不需要管理员终端。

成功后固定地址会写入：

```toml
[remote_access]
enabled = true
port = 8080
url = "https://windows-host.example.ts.net"
```

配置文件位于当前用户的 `~/.ai-brain/config.toml`（PowerShell 中通常是 `$HOME\.ai-brain\config.toml`）。设备改名或 Tailnet DNS 后缀变化时，智脑会在下次成功启动时更新 `url`。如需只允许本机访问，将 `enabled` 改为 `false` 后重启智脑。

`ai-brain.exe remote --port 8080` 仍保留为严格远程模式和排障命令；日常常驻启动使用 `web`。

## 六、Windows 主机安全检查

另开一个 PowerShell：

```powershell
Get-NetTCPConnection -State Listen -LocalPort 8080 |
    Select-Object LocalAddress,LocalPort,OwningProcess

tailscale serve status
```

必须满足：

- 智脑监听地址是 `127.0.0.1:8080`。
- 不能出现 `0.0.0.0:8080` 或对外网卡地址。
- Tailscale Serve 的目标是 `http://127.0.0.1:8080`。
- 不配置路由器端口转发，不为 8080 建立公网入站规则。

普通 `web` 自动远程模式下，本机仍可访问 `http://127.0.0.1:8080`。只有显式 `remote` 严格模式会要求所有请求都经过 Tailscale Serve。

## 七、手机端验证

1. 打开手机 Tailscale，确认状态为已连接。
2. 使用手机浏览器打开终端打印的 `https://...ts.net` 地址。
3. 确认能看到会话列表，并显示 WebSocket 已连接。
4. 发送低风险测试消息，例如“只回复 remote-connected”。
5. 新建会话、切换会话，再刷新页面，确认历史仍存在。
6. 切换一次 Wi-Fi/蜂窝网络，确认页面能重新连接。
7. 在明确授权后，再测试读取工作区文件；不要先测试删除或任意 Bash 命令。

## 八、停止远程访问

在智脑窗口按 `Ctrl+C` 停止 AI Brain。Tailscale Serve 使用后台配置，下一次 `web` 启动会自动恢复；若要关闭自动远程访问，先把配置中的 `remote_access.enabled` 改为 `false`。若要同时取消当前代理，再执行：

```powershell
tailscale serve off
```

确认：

```powershell
tailscale serve status
Get-NetTCPConnection -State Listen -LocalPort 8080 -ErrorAction SilentlyContinue
```

## 九、Windows 上交给 Codex 的任务说明

在 Windows 工作区打开 Codex 后，可以直接发送：

```text
请读取 docs/windows-remote-access.md，并在当前 Windows 主机上完成智脑远程模式的原生验证。

要求：
1. 先确认当前分支和工作区改动，不覆盖已有修改。
2. 检查 Tailscale Windows 服务和 tailscale.exe 实际路径。
3. 运行文档中的格式、远程单测、确定性全量测试和 release 构建。
4. 启动 ai-brain.exe web，确认它自动恢复 Tailscale Serve 并打印固定 URL。
5. 验证只监听 127.0.0.1，并检查 tailscale serve status。
6. 根据真实 Windows 错误修改代码并补测试，不用 Mac 交叉编译结果代替实机结果。
7. 给出手机访问 URL，并协助完成手机浏览器端到端测试。
8. 不开放 0.0.0.0，不配置公网端口转发，不使用 Tailscale Funnel。
```

## 十、出现问题时保留的信息

把以下命令输出交给 Windows 上的 Codex；发送前可遮盖邮箱、设备名和 Tailnet 名称：

```powershell
$PSVersionTable.PSVersion
[System.Environment]::OSVersion.VersionString
rustc -Vv
cargo -V
Get-Command tailscale -ErrorAction SilentlyContinue
Get-Service Tailscale
tailscale version
tailscale status
tailscale serve status
Get-NetTCPConnection -State Listen -LocalPort 8080 -ErrorAction SilentlyContinue
git status --short
git branch --show-current
```

同时保留失败命令的完整错误文本。不要发送 API Key、OAuth Token、Tailscale Auth Key 或其他凭据。
