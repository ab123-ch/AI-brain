# 小说运行时可靠性修复设计

## 目标

修复当前 Novel Writer 因输出合同未传达而稳定失败的问题，补齐失败响应诊断、Web 文件日志、Novel 工具入口校验和可行动错误，并在不改变显式模型选择、不放宽 Canon/资源完整性权限的前提下完成 release 重建、现场重启和一次最小付费 DeepSeek smoke。

## 已确认故障

- 生产任务 `wupo-guize-ch1-body-001` 使用 `deepseek/deepseek-v4-pro`，Provider 返回可解析 JSON，但 Rust 反序列化因缺少 `content` 失败。
- 运行时冻结 Profile 只有 `schema_id = novel.writer-output.v1`，Writer prompt 没有完整 JSON 结构；`ChatRequest` 也没有协议层 response schema。
- Writer 原始响应只在解析成功后进入 `NovelWriterExecution`。解析失败时数据库只保存反序列化错误，无法看到响应形态、finish reason 或内容 hash。
- Web/非 TUI 模式先初始化终端 subscriber，再尝试初始化文件 subscriber；第二次全局初始化失败被忽略，导致当天 `brain-*.log` 为零字节。
- `resume.input` 已在工具 JSON Schema 中声明 required，ContextRef hash 也已严格校验并返回 actual hash。生产中的缺参和 stale hash 属于模型违反 schema 或使用旧摘要，不能通过放宽领域校验解决。
- 重启前的 Gemini 任务曾出现 400 参数错误、网络失败、403 额度不足和 503 模型无渠道。用户明确要求：显式 Provider 失败时直接告知，不允许自动回退到 DeepSeek。

## 方案选择

采用“契约优先、局部修复”方案：在 Novel Workflow/Adapter 边界定义并发送完整版本化合同，严格解析并允许同一模型做一次有预算约束的格式纠正。

不采用以下方案：

- 不扩展所有 `brain-llm` Provider 的通用 `response_format/json_schema`。当前代理实现和模型兼容性不同，改动面超过本次 Novel 故障边界。
- 不通过字段别名、任意层级搜索或缺省 outcome 放宽解析器。该做法会把合同漂移隐藏成成功，削弱可审计性。
- 不在 Gemini/DeepSeek 或任意 Provider 之间自动回退。显式模型选择、费用和输出语义必须保持稳定。

## Writer 输出合同

`novel-workflow` 提供 `novel.writer-output.v1` 的完整 prompt 合同，`ai-brain-cli` 的 Writer Adapter 直接复用，不另写一份漂移副本。

合同只允许两种根对象。

`draft_ready` 示例：

```json
{
  "outcome": "draft_ready",
  "draft": {
    "content": "完整候选正文",
    "self_review": {
      "verdict": "pass",
      "checks": {
        "outline_alignment": "pass",
        "canon_consistency": "pass",
        "character_consistency": "pass",
        "timeline_consistency": "pass",
        "plot_and_foreshadowing": "pass",
        "style_and_repetition": "pass"
      },
      "issues": [],
      "unverified_assumptions": [],
      "summary": "六项检查通过"
    },
    "proposed_delta": {
      "project_id": "project-1",
      "branch_id": "main",
      "expected_revision": 5,
      "task_type": "body",
      "source_ref": "chapters/0001.md",
      "progress": null,
      "proposed_facts": [],
      "state_changes": [],
      "plot_updates": [],
      "foreshadowing_updates": [],
      "feedback": [],
      "experience_candidates": []
    },
    "evidence_refs": ["outline.md#sha256:32d2bb2313014d2e89e73d69a2c1b9ff390b76e887420812a32663bcf08d951a"]
  }
}
```

Adapter 生成 prompt 时把示例中的 `project_id`、`expected_revision`、`task_type`、`source_ref` 和 evidence refs 替换为冻结任务的真实值，避免模型猜测领域标识。

`needs_clarification` 示例：

```json
{
  "outcome": "needs_clarification",
  "questions": ["需要用户补充的具体问题"],
  "reason": "为什么缺少该信息会阻止可靠创作"
}
```

解析器要求显式 `outcome`。不再把任意合法 JSON 默认解释为 `draft_ready`；未知 outcome、缺字段、空正文、自检未通过、project/revision 不一致均保持失败。

## 同模型格式纠正

首次 Provider 调用成功但合同解析失败时，Adapter 最多追加一次格式纠正请求：

1. 使用原 Provider 和原 model，不改变 temperature 策略，不启用工具。
2. 把第一次响应作为 assistant 内容，并要求只重排为完整合同，不新增、删改正文事实。
3. 纠正调用的 `max_tokens` 不超过 Writer 剩余输出预算；没有剩余预算时直接失败。
4. 纠正成功后合并两次 Provider 的真实 input/output token usage，再交给 TaskEngine 结算。
5. Provider 网络/API 错误不进入格式纠正，也不触发跨 Provider 回退。
6. 第二次仍不合规时结束任务，不做第三次尝试。

## 失败响应诊断

Adapter 为每次不合规响应生成受限诊断：

- Provider 名、model、尝试序号和 finish reason。
- 完整原始响应的 SHA-256。
- 最多 2,048 个 Unicode 字符的预览。
- 对 `Authorization`/`Bearer` 值、`api_key`/`token`/`secret` 键值以及 `sk-` 开头的长 token 做脱敏，并把控制字符转义为可打印文本。

诊断随 `InvalidWriterOutput` 进入现有 `instance_runs.error`、`node_failed` 和 `task_failed` 持久事件，并以 `warn` 写入 tracing。正文原文不完整复制到错误字段，避免数据库和日志无限增长。成功 Artifact 的内容与权限边界不变。

## Provider 错误语义

Writer Provider 调用错误必须包含 provider/model 路由和原始错误分类。额度不足、模型无渠道、参数错误等不可由 Novel Adapter 改写为格式错误，也不得自动切换模型。网络层既有的同 Provider 有界重试保持不变；耗尽后直接向调用者报告。

## Novel 工具入口

- 在任何 `associate_conversation_source` 或领域调用前，先完成 action-specific 反序列化和非空字段校验。
- `resume` 只接受非空 `task_id` 与 `input`；缺参返回稳定错误码、所需字段和“仅用于 needs_clarification”的恢复说明。
- ContextRef hash 校验继续比较 expected/actual。ContextChanged 错误附带恢复说明：重新读取当前文件、使用 actual SHA-256、沿用同一 task ID 最多重试一次。
- Novel 写作工作流 prompt 明确禁止对同一 stale hash 连续重试，禁止在非 `needs_clarification` 状态调用 `resume`。
- 不引入绕过资源 scope、用户接受、主脑复审或 Canon publication authority 的路径。

## 日志初始化

用一个统一入口替代“先终端 init、再文件 try_init”：

- TUI：一个 registry，只挂文件 layer。
- Web/CLI：一个 registry，同时挂 EnvFilter 终端 layer 和 DEBUG 文件 layer。
- 文件打开或 subscriber 初始化失败时返回明确错误；非 TUI 可回退到终端并打印错误，TUI 回退到 sink 但在进入 alternate screen 前打印错误。
- 把 subscriber 组装与全局安装分离，使测试能用局部 default subscriber 验证文件确实收到事件，避免测试进程重复初始化全局 subscriber。

## 测试策略

严格按红—绿顺序添加以下覆盖：

- Writer prompt 包含完整 `draft_ready`/`needs_clarification` 字段以及冻结 project/revision/task type。
- 任意合法但无 outcome/content 的 JSON 明确失败。
- 首次错误、第二次合规时只纠正一次并累计 usage。
- Provider 失败不纠正、不回退；两次格式失败记录 hash、finish reason 和限长脱敏预览。
- `resume` 缺少或提供空 input 时，在会话关联和领域服务调用前失败。
- ContextChanged 错误保留 expected/actual 并提供单次恢复动作。
- 非 TUI subscriber 同时写终端/文件，TUI 文件 layer 可写，初始化失败可见。
- 现有 Novel Domain/Workflow/Application、tools、CLI 回归保持通过。

最终执行 `cargo fmt`、受影响 crate 的严格 Clippy、受影响测试、`cargo clippy --workspace --all-targets -- -D warnings` 和 `cargo test --workspace`。若全 workspace 被已知无关债务阻塞，必须保存完整输出并单独证明所有本次修改边界通过，不得把阻塞误报为通过。

## 隔离、合并与现场部署

- 在 `fix/novel-runtime-reliability` linked worktree 中实现和提交，主检出目录的未跟踪小说材料不参与提交。
- 通过全部门禁后，把限定提交合并回 `featrue/20260404-nao`；不 push。
- 在主检出目录重建 `rust/target/release/ai-brain.exe`。
- 重启前记录当前 PID、监听地址、数据库计数和 WAL 状态；只停止已核实命令行为当前仓库 release Web 服务的进程。
- 用隐藏窗口启动新 release，显式重定向 stdout/stderr，验证 PID、端口、HTTP、WebSocket 与当天非空文件日志。
- 创建独立 smoke project/task ID，通过当前 `deepseek-v4-pro` 运行最小 Writer 合同。只验证 `draft_ready` Artifact 与任务终态，不调用 decide/publish，不修改正式 Canon。
- 若 smoke 失败，保留服务、数据库和诊断证据，不循环付费重试。

## 非目标

- 不修复或充值外部 Gemini 渠道。
- 不增加跨 Provider 自动回退。
- 不删除历史失败任务、旧日志或现有小说数据。
- 不放宽用户接受、主脑复审、资源 hash、工作区 scope 或 publication 权限。
- 不顺手重构与 Novel Writer、日志或工具入口无关的组件。
