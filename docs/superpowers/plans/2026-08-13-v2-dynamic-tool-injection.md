# 智脑 v2 动态工具注入总执行计划 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让智脑 v2 的模型可见工具全部由运行时 Provider 动态发现和注入，且工具定义、权限、健康状态与真实执行路由始终来自同一个请求快照。

**Architecture:** `brain-core` 定义对象安全的 Provider/Route/Snapshot 合同，`brain-motor` 实现按 Provider 原子替换的版本化注册中心，MainBrain 在每次请求开始时固定一个不可变快照。Builtin、MCP、Plugin、Session 依次迁入同一注册中心；ToolSearch、Motor、Eval、Evolver 和 Web 状态接口只消费该中心的派生视图。

**Tech Stack:** Rust 2021、Tokio、serde/serde_json、rmcp 3.1.2、reqwest、Axum、Cargo workspace tests。

---

## 文件结构与规格关系

- 批准的设计规格：`docs/plans/2026-08-13-v2-dynamic-tool-injection-design.md`
- 设计提交：`408c0488 docs: design v2 dynamic tool injection`
- 子计划 A：`docs/superpowers/plans/2026-08-13-v2-tool-registry-mainbrain.md`
- 子计划 B：`docs/superpowers/plans/2026-08-13-v2-builtin-consumers-paths.md`
- 子计划 C：`docs/superpowers/plans/2026-08-13-v2-mcp-plugin-skills.md`
- 子计划 D：`docs/superpowers/plans/2026-08-13-v2-websearch-cleanup-verification.md`

四份子计划必须按 A → B → C → D 执行。每份结束时都保持工作区可编译、关键测试通过，并产生小而可审查的提交；不得把后续阶段的失败测试提前留在主分支。

## 文件结构总览

| 文件/目录 | 责任 |
|---|---|
| `rust/crates/brain-core/src/tool_registry.rs` | Provider、Route、Registration、Snapshot、RequestView、策略和结构化错误合同 |
| `rust/crates/brain-core/src/tool_policy.rs` | 共享权限上限、基于 canonical metadata 的参数 guard 与确认决策 |
| `rust/crates/brain-core/src/paths.rs` | `AI_BRAIN_HOME`、HOME/USERPROFILE、用户/项目配置路径的唯一解析器 |
| `rust/crates/brain-motor/src/tool_registry.rs` | `DynamicToolRegistry` 的 Provider 原子替换、版本递增、冲突诊断与派生快照 |
| `rust/crates/brain-main/src/main_brain.rs` | 请求开始时固定快照；同步、流式、压缩重入和 fork 共用该快照 |
| `rust/crates/brain-main/src/tool_loop.rs` | 从同一 RequestView 广告、校验、授权、执行 route，并动态激活 deferred 工具 |
| `rust/crates/tools/src/provider.rs` | `BuiltinProvider` 与按真实依赖/平台判断可用性的注册记录 |
| `rust/crates/tools/src/web.rs` | WebSearch/WebFetch 的有界 HTTP 实现与搜索后端门面 |
| `rust/crates/brain-mcp/src/` | 统一 MCP 配置、rmcp 客户端、stdio/Streamable HTTP、资源、认证、刷新与 Provider |
| `rust/crates/plugins/src/provider.rs` | 启用插件工具到限定名注册记录的适配与 route |
| `rust/crates/brain-plugin/src/skill_registry.rs` | 版本化 `SkillCatalogSnapshot` 与失败保留上一健康版本 |
| `rust/crates/ai-brain-cli/src/tool_runtime.rs` | Provider 组装、reload/status/shutdown 和 Orchestrator 适配 |
| `rust/crates/ai-brain-cli/src/web/collaboration_tools.rs` | 群会话专属 `SessionProvider` 注册记录 |
| `rust/crates/brain-eval/src/eval_brain.rs` | 从全局快照派生只读 PolicyView |
| `rust/crates/brain-evolver/src/web_search.rs` | 异步 SearchCapability adapter；无能力时明确失败 |
| `rust/crates/ai-brain-cli/src/api_server.rs` | v2 Web 的工具状态与显式 reload 接口 |
| `rust/crates/brain-integration-tests/tests/dynamic_tools.rs` | 跨 crate 的生产组装、请求快照、热刷新与伪工具排除合同 |

实现者必须先读上面的设计规格和四份子计划；下表只锁定跨子系统边界，具体 Create/Modify 文件、RED 测试代码、最小实现片段、命令和提交拆分以对应子计划为准。总计划不复制四份实现片段，避免同一合同出现两套版本。

## 不可破坏的合同

1. 内置兼容名称与 Schema 保持稳定；MCP 名称为 `mcp__<server>__<tool>`，插件名称为 `plugin__<plugin>__<tool>`。
2. 每条模型可见记录同时拥有 descriptor、metadata 和真实 route；没有 route 的能力不得进入快照。
3. 一个请求只固定一个基础 `Arc<ToolSnapshot>`；ToolSearch 只能扩大该请求的广告集合，不能切换到新 registry version。
4. 未广告、未知、不可用或超权限的调用在 route 前拒绝；`AskUserQuestion` 也必须先通过广告校验。
5. Provider 刷新先在锁外完成 I/O，再在短临界区提交完整候选；失败不暴露半成品。
6. 生产环境不注册 LSP、RemoteTrigger、TestingPermission 或 MCP 兼容门面，除非对应真实 Provider 已初始化。
7. 动态工具不能绕过工作目录、sandbox、参数 guard 或 hooks。
8. Windows 没有 `sh` 时不广告 bash；存在真实 PowerShell 时可广告 PowerShell。
9. 所有日志和状态结构不得包含 token、Authorization、OAuth code 或完整敏感 header。
10. registry/version 只在语义状态变化时递增；相同 fingerprint 的 reload 返回 `changed=false`，不能为了满足测试伪造版本增长。

### Task 1: 建立隔离执行基线

**Files:**
- Read: `AGENTS.md`
- Read: `docs/plans/2026-08-13-v2-dynamic-tool-injection-design.md`
- Read: 本计划与四份子计划
- Preserve: `.idea/vcs.xml`, `rust/.clawd-todos.json`, `rust/crates/brain-llm/src/openai_compat.rs` 及所有既有未跟踪文件

- [ ] **Step 1: 调用隔离工作区技能并创建实现 worktree**

执行前使用 `using-git-worktrees`，从包含设计与本实施计划的当前 `HEAD` 创建专用实现分支；`HEAD` 必须是 `408c0488` 的后代。不得移动、stash、覆盖或提交当前工作区的用户修改。

- [ ] **Step 2: 记录基线状态**

Run:

```powershell
git rev-parse --show-toplevel
git rev-parse HEAD
git status --short --branch
cargo test -p brain-core -p brain-motor -p brain-main -p brain-mcp -p tools -p plugins -p brain-plugin -p brain-eval -p brain-evolver
```

Working directory: 新 worktree 的 `rust/`

Expected: HEAD 包含 `408c0488` 和本实施计划提交；窄版基线通过。若仓库既有失败，原样记录测试名和输出，先证明与本任务无关再继续。

- [ ] **Step 3: 建立执行日志**

在 worktree 根目录的 `progress.md` 追加：worktree 路径、分支、HEAD、基线命令和结果。仅追加本任务段落，不覆盖已有记录。

### Task 2: 执行子计划 A——核心注册表与 MainBrain

**Files:**
- Follow: `docs/superpowers/plans/2026-08-13-v2-tool-registry-mainbrain.md`

- [ ] **Step 1: 逐任务执行 RED → GREEN → commit**

严格执行子计划 A 的 8 个任务；每个测试必须先观察到预期失败，再写最小实现。

- [ ] **Step 2: 运行阶段门禁**

Run:

```powershell
cargo test -p brain-core -p brain-motor -p brain-main
cargo test -p brain-main pinned_snapshot --lib
cargo fmt --check
```

Expected: 全部通过；生产 Orchestrator 仍可通过过渡 adapter 启动，但 MainBrain 新路径已只接受快照。

### Task 3: 执行子计划 B——Builtin、消费者、路径与权限

**Files:**
- Follow: `docs/superpowers/plans/2026-08-13-v2-builtin-consumers-paths.md`

- [ ] **Step 1: 逐任务执行 RED → GREEN → commit**

严格执行子计划 B 的 8 个任务，完成 BuiltinProvider、动态 ToolSearch、Motor/Eval、SessionProvider、Windows 路径和 shell 可用性迁移。

- [ ] **Step 2: 运行阶段门禁**

Run:

```powershell
cargo test -p tools -p brain-motor -p brain-eval -p brain-main
cargo test -p ai-brain-cli collaboration --lib
cargo test -p brain-integration-tests --test dynamic_tools
cargo fmt --check
```

Expected: 全部通过；v2 生产入口不再调用 `mvp_tool_definitions()`，但 MCP/插件仍可处于“未连接且不广告”的真实状态。

### Task 4: 执行子计划 C——真实 MCP、插件与技能刷新

**Files:**
- Follow: `docs/superpowers/plans/2026-08-13-v2-mcp-plugin-skills.md`

- [ ] **Step 1: 逐任务执行 RED → GREEN → commit**

严格执行子计划 C 的 10 个任务。依赖必须固定为：

```toml
rmcp = { version = "3.1.2", default-features = false, features = [
  "client",
  "transport-child-process",
  "transport-streamable-http-client-reqwest",
  "reqwest",
  "auth",
  "which-command",
] }
```

`ClientLifecycleMode::Auto` 的 preferred versions 必须显式以 `V_2026_07_28` 开头，不能使用仍指向 2025-11-25 的 `ProtocolVersion::LATEST`。

- [ ] **Step 2: 运行阶段门禁**

Run:

```powershell
cargo tree -p brain-mcp -e features
cargo test -p brain-mcp -p runtime -p plugins -p brain-plugin
cargo test -p ai-brain-cli mcp --lib
cargo test -p ai-brain-cli plugin --lib
cargo fmt --check
```

Expected: `cargo tree` 显示 rmcp 3.1.2 及六个目标 feature；stdio、HTTP、旧通知、新订阅、资源、插件启停和技能刷新测试全部通过。

### Task 5: 执行子计划 D——WebSearch/Evolver、清理与验收

**Files:**
- Follow: `docs/superpowers/plans/2026-08-13-v2-websearch-cleanup-verification.md`

- [ ] **Step 1: 逐任务执行 RED → GREEN → commit**

严格执行子计划 D 的 8 个任务，完成 WebSearch backend/fallback、Evolver adapter、伪工具删除、管理 API、静态扫描与 Release 烟测脚本。

- [ ] **Step 2: 运行完整自动化门禁**

Run:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python -m unittest tests.test_porting_workspace
cargo build --release -p ai-brain-cli
git diff --check
```

Working directory: Rust 命令从 `rust/` 执行；Python 命令和 `git diff --check` 从仓库根执行。

Expected: 全部退出 0。Python 归档工具快照保持原样通过，证明本次没有用动态 v2 改造破坏旧镜像面。

### Task 6: 独立规格/质量复核与真实 v2 烟测

**Files:**
- Verify: `docs/plans/2026-08-13-v2-dynamic-tool-injection-design.md`
- Verify: 所有本任务提交
- Use: `scripts/smoke-v2-dynamic-tools.ps1`

- [ ] **Step 1: 使用 requesting-code-review 做规格覆盖复核**

复核者逐条检查设计第 19 节九项完成定义，并报告 Critical / Important / Minor。任何 Critical/Important 必须先补 RED 测试再修复；修复后重新复核。

- [ ] **Step 2: 在干净配置目录启动 Release**

Run:

```powershell
$env:AI_BRAIN_HOME = Join-Path $env:TEMP 'ai-brain-dynamic-tools-smoke'
& .\scripts\smoke-v2-dynamic-tools.ps1 -Binary .\rust\target\release\ai-brain.exe -Port 18080
```

Working directory: 仓库根

Expected: 脚本创建隔离配置、启动服务、验证 builtin 调用、registry version/status、reload 和干净关闭，并删除自己创建的临时目录。

- [ ] **Step 3: 使用当前用户 open-websearch 配置做真实 stdio MCP 冒烟**

Run:

```powershell
& .\scripts\smoke-v2-dynamic-tools.ps1 `
  -Binary .\rust\target\release\ai-brain.exe `
  -Port 18081 `
  -UseCurrentUserConfig `
  -ExpectedMcpServer 'open-websearch'
```

Expected: 状态中出现健康的 `open-websearch`，至少一个 `mcp__open_websearch__*` 工具被发现并真实调用；输出不是空数组、stub 或 “not connected”。若第三方服务真实不可用，脚本必须把 Provider 标记为错误且不广告工具，不能假成功。

- [ ] **Step 4: 验证热刷新和请求快照**

在脚本 fixture 中启动一个故意阻塞的旧请求，更新 MCP/插件配置并调用 reload：旧请求仍可完成旧 route，新请求看到新 version 和新工具；禁用插件/断开 MCP 后下一版撤销对应工具。

- [ ] **Step 5: 停止旧服务并替换为已验证 Release**

先确认当前监听 `127.0.0.1:8080` 的 PID 与可执行文件属于本项目，再正常请求关闭；若没有关闭 API，则只终止该精确 PID。随后用已验证 Release 启动并检查 HTTP 200、`/api/tools/status` 与首页 WebSocket。

- [ ] **Step 6: 最终提交与交付**

Run:

```powershell
git status --short
git log --oneline --decorate 408c0488..HEAD
git diff --check 408c0488..HEAD
```

Expected: 只有本任务预期文件；用户原工作区修改未进入提交。最终报告列出提交、测试证据、真实 MCP/插件/WebSearch 状态和仍未配置因而未广告的能力。
