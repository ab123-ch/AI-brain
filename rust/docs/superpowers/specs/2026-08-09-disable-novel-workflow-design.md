# 停用 Novel 工作流设计

## 背景

当前 Novel 工作流把普通小说请求转交给独立的 `deepseek-v4-pro` Writer，并额外引入项目状态机、任务锁、候选稿复审、发布和 Canon 投影。用户希望恢复为当前会话所选 Gemini 直接读取资料、生成正文和写入文件，同时保留现有 Novel 源代码以便将来人工恢复。

## 决策

不新增配置开关，也不删除 Novel crate、领域模型、应用服务、数据库代码或测试。直接注释掉生产运行时的 Novel 接线和模型可见入口，并在注释处写明停用原因与恢复位置。

停用后的行为：

1. 主脑不再看到 `novel_project`、`novel_task` 工具。
2. 主脑不再被提示必须加载 `novel-writing-workflow` Skill。
3. 启动时不再创建 Novel Writer、不再把 `main` 路由复用为独立 Writer，也不启动 Novel projection worker。
4. 小说请求由当前会话选择的模型直接处理，并仅使用普通 `read_file`、`write_file`、`edit_file`、搜索等通用工具。
5. 不允许静默回退到 DeepSeek Novel Writer；普通工具或当前模型失败时直接返回真实错误。

## 保留内容

- `novel-domain`、`novel-application`、`novel-workflow`、`novel-knowledge-adapter` 和相关测试继续保留。
- `RealToolExecutor` 中的 Novel facade 实现继续保留，但生产实例不注入 Novel application，且工具清单不再暴露对应入口。
- `novel-writing-workflow` Skill 源文件继续保留，但生产启动不再安装或强制加载。
- 现有 `novel.db`、`runtime.db` 中的历史任务、候选稿、审计记录以及工作区小说文件保持原样。
- 已存在的活动任务变为不可从主脑访问的历史状态；不自动完成、解锁、发布或删除。

## 生产接线变更

### 工具清单

在生产 ToolSpec 注册处注释 `novel_task` 和 `novel_project` 两个条目，并保留带原因的恢复注释。普通文件工具保持可见。

工具执行器中的 Novel 实现不删除，避免丢失已验证的领域代码；由于工具不注册且生产不注入 application，主脑无法调用该路径。

### Orchestrator

注释以下生产初始化链路：

- Novel 数据库打开与旧数据迁移；
- Scoped Novel resource adapter；
- Novel Writer client 和 workflow；
- Novel application port 注入；
- Novel projection worker；
- 启动时安装 Novel Skill。

保留源码与测试所需类型。生产 Orchestrator 不再持有必须可用的 Novel application；所有非 Novel 功能继续正常启动。

### 主脑提示词

注释强制小说请求进入 Novel facade 的系统提示。保留通用文件工具和当前模型的正常写作能力，不增加另一套隐藏状态机或自动路由。

## 数据与安全

本变更不删除任何数据。启动和普通写作不得修改 `novel.db`。若用户将来决定恢复，只能人工取消接线注释、重新构建并重启；旧任务是否继续使用必须另行决定，不能自动恢复执行。

直接写作失去原工作流提供的并发锁、Canon 提交、候选稿封存和受控发布。文件覆盖安全继续由普通文件工具及用户确认负责。

## 验证

实现必须用自动化测试证明：

1. 生产工具清单不包含 `novel_task`、`novel_project`。
2. 系统提示不再要求加载 Novel Skill 或使用 Novel facade。
3. 生产 Orchestrator 启动不创建 Novel Writer、不要求其 Provider/API Key，也不写入 Novel 数据库。
4. `read_file`、`write_file`、`edit_file` 仍正常注册和执行。
5. 现有 Novel crate 的定向测试仍可独立编译运行，证明代码只是停用而非删除。
6. 格式检查、相关单元测试和定向 Clippy 通过；不调用付费 Provider 做冒烟测试。

## 不在本次范围

- 删除 Novel 源代码、Cargo workspace 成员或数据库；
- 修改或发布当前候选稿；
- 清除当前活动任务；
- 调整 Gemini/DeepSeek API Key；
- 新增 hash 缓存或新的小说状态机。
