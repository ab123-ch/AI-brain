# 全脑对齐修复计划

> 日期: 2026-04-15
> 基准文档: `docs/plans/2026-04-09-phase11-development-design.md`
> 目标: 将实际实现与设计文档对齐，按优先级逐步修复

---

## 修复原则

1. **主脑是瓶颈** — 主脑不重构，其他脑的高层功能无处接入
2. **从内到外** — 先改主脑架构（通信模式+三阶段），再改各脑缺失功能
3. **每步可验证** — 每个 Step 完成后 `cargo test --workspace` + `cargo clippy` 全通过
4. **不破坏现有测试** — 旧测试必须继续通过，新测试覆盖新功能

---

## Step 1: 主脑架构重构 — 直接调用模式

**优先级**: P0（后续所有 Step 的前提）

### 1.1 MasterBrain 结构体重构

**设计要求** (`design.md:433-439`):
```rust
struct MasterBrain {
    sensory: Arc<SensoryBrain>,
    reasoning: Arc<ReasoningBrain>,
    memory: Arc<MemoryBrain>,
    llm: Arc<dyn LlmProvider>,
    config: BrainConfig,
    // 异步事件通道（保留，用于非阻塞通知）
    event_rx: mpsc::Receiver<BrainEvent>,
}
```

**实际**: 只有 `Arc<BrainBus>` + `WeightEngine`

**改动**:
- `master.rs`: 新增 `sensory`, `reasoning`, `memory`, `llm` 字段
- `new()` 接受各副脑引用和 LLM Provider
- 保留 `BrainBus` 用于事件通知（降级为辅助）
- 保留 `WeightEngine`（权重进化仍有价值）

### 1.2 废弃 run_once() 广播→收集模式

**设计要求** (`design.md:414-428`):
> 当前 brain-bus 的广播→收集模式不适合新架构。新架构采用直接方法调用为主、消息通道为辅的模式。

**改动**:
- `run_once()` 重命名为 `run_task()` — 三阶段完整流程
- 保留旧的广播→收集逻辑作为 fallback（简单查询场景）
- `run_task()` 流程：`sensory.perceive()` → 澄清循环 → 执行循环

### 1.3 接入 LLM Provider

**设计要求** (`design.md:206-207`):
> let llm_score = self.llm_quality_score(step, result).await;

**改动**:
- 主脑持有 `Arc<dyn LlmProvider>`
- `evaluate_step()` 改为 `async`，增加 LLM 质量评分
- `synthesize()` 改为 `async`，LLM 汇总各副脑响应

### 1.4 用户交互接口

**设计要求** (`design.md:219-224`):
- 澄清阶段：汇总问题，统一呈现给用户
- 计划确认：呈现完整 TaskPlan
- 危险操作：guard_check 拦截后询问用户
- 步骤失败：告知失败原因，提供选项

**改动**:
- 新增 `UserInteraction` trait（抽象用户交互，CLI/REPL 实现不同）
- `ask_user(question) -> String`
- `confirm_action(action) -> bool`
- `choose_option(prompt, options) -> usize`
- `report_progress(message)`
- REPL 端实现：stdin/stdout 交互

**验收**: `cargo test --workspace` 全通过，主脑可持有副脑引用并直接调用

---

## Step 2: 三阶段模型实现

**优先级**: P0（主脑重构完成后立刻做）

### 2.1 Phase 1: 感知阶段

**设计要求** (`design.md:48-56`):
> 用户输入 → 感知脑（LLM 理解 + 拆解 + 推荐工具）
> 输出: TaskPlan { status: Draft, steps[], tools[] }

**改动**:
- 主脑 `run_task()` 第一步：调用 `sensory.decompose(input)`
- 获取 `TaskPlan`，状态为 `Draft`
- 检查 `plan.known_ambiguities` 是否非空，是则进入 Phase 2

### 2.2 Phase 2: 澄清循环

**设计要求** (`design.md:159-172`):
```
while plan.status != Confirmed {
    reasoning.check_ambiguity(plan)   // 推理脑检查歧义
    evaluation = self.evaluate(plan)  // 主脑评估
    if questions.is_empty() {
        plan.status = Confirmed
    } else {
        answers = user.confirm(questions)  // 问用户
        plan.update(answers)               // 更新计划
        memory.record(questions, answers)  // 记忆脑记录
    }
}
```

**改动**:
- 主脑新增 `clarification_loop(plan) -> Result<TaskPlan>`
- 调用 `reasoning.check_ambiguity(plan)` （已有实现但未接入，在 `tool_loop.rs:168-201`）
- 接入 `evaluate_plan_readiness()` （已实现）
- 通过 `UserInteraction` 呈现问题、收集回答
- 调用 `memory.extract_facts(qa_pairs)` （已有实现但未接入）
- 循环终止条件：推理脑确认无歧义 / 主脑评估达标 / 用户说"开始执行"

### 2.3 Phase 3: 执行循环

**设计要求** (`design.md:173-186`):
```
for step in plan.steps {
    memory.inject(step)                      // 记忆脑注入
    result = reasoning.execute_step(step)    // 推理脑执行
    eval = self.evaluate_step(step, result)  // 主脑评估
    if eval.pass {
        memory.persist(step, result)          // 记忆脑持久化
        memory.compress(step)                 // 记忆脑压缩
        step.status = Done
    } else {
        // 重试或调整，告知用户
    }
}
```

**改动**:
- 主脑新增 `execution_loop(plan) -> Result<Vec<StepResult>>`
- 每步：先调记忆脑注入 → 再调推理脑执行 → 评估 → 持久化/重试
- `evaluate_step()` 升级为 async + LLM 质量评分
- 失败时通过 `UserInteraction` 告知用户，提供选项（重试/跳过/调整）

**验收**: 主脑可以完成 感知→澄清→执行 的完整三阶段流程

---

## Step 3: 感知脑补全 — 资源池查询与工具推荐

**优先级**: P1

### 3.1 资源池查询

**设计要求** (`design.md:104-111`):
- 扫描技能库（SKILL.md 的 name + description）
- 查询已注册的 MCP 服务器及其工具列表
- 查询已安装的插件

**改动**:
- 新增 `ResourcePool` 结构体（或直接在 SensoryBrain 内实现）
- `scan_skills() -> Vec<ToolRef>`：扫描 `~/.ai-brain/skills/` 下的 SKILL.md
- `scan_mcp_tools() -> Vec<ToolRef>`：查询 McpServerManager
- `scan_plugins() -> Vec<ToolRef>`：查询已注册插件

### 3.2 Top 5 工具推荐

**设计要求** (`design.md:109-111`):
> [TDD skill(0.9), filesystem MCP(0.85), bash MCP(0.8), ...]

**改动**:
- 在 `decompose()` 中，LLM prompt 加入资源池信息（`{resource_pool}` 占位符）
- 解析 LLM 返回的 `recommended_tools` 字段
- `ToolRef` 结构体已有位置：`brain-core/src/plan.rs`

**验收**: 感知脑输出的 TaskPlan 包含有意义的 recommended_tools 列表

---

## Step 4: 推理脑桥接 — 接入记忆脑

**优先级**: P1

### 4.1 持有 MemoryBrain 引用

**设计要求** (`design.md:451`):
> 推理脑 | 记忆脑 | 直接调用 | memory.recall_full(ref_id)

**改动**:
- `ReasoningBrain` 新增 `memory: Option<Arc<MemoryBrain>>` 字段
- 新增 `set_memory()` 方法
- `brain-reasoning/Cargo.toml` 添加 `brain-memory` 依赖

### 4.2 记忆召回集成到 tool_loop

**设计要求** (`design.md:387`):
> 推理脑永远不需要主动问"之前决定了什么"——记忆脑保证这些信息始终在推理脑的上下文中

**改动**:
- `tool_loop::execute_step()` 在每次 LLM 调用前，注入记忆脑的上下文
- 调用 `memory.recall_for_context(step)` 获取分级注入内容
- 记忆注入内容追加到 LLM 消息列表末尾（保护缓存前缀）
- 推理脑可通过 `memory.recall_full(ref_id)` 请求完整记录

### 4.3 check_ambiguity() 接入澄清循环

**现状**: `tool_loop.rs:168-201` 已实现但从未被调用

**改动**:
- 在 Step 2 的澄清循环中调用（主脑调推理脑）
- 确保推理脑使用澄清模式 system prompt（`design.md:1014-1050`）

**验收**: 推理脑 tool_loop 中能看到记忆注入内容，check_ambiguity 被澄清循环调用

---

## Step 5: 记忆脑高层功能接入

**优先级**: P2（依赖 Step 2 的执行循环）

### 5.1 权重召回分级注入

**现状**: `MemoryScore` 类型已定义，`ContextBuilder` 已实现，但未被调用

**改动**:
- `MemoryBrain.recall_for_context()` 内部调用 `ContextBuilder`
- 实现 `MemoryScore` 四维评分（relevance/recency/importance/frequency）
- 分级注入策略：≥0.8 完整 / 0.5-0.8 摘要 / 0.3-0.5 提及 / <0.3 跳过
- Token 预算控制（默认 4000）

### 5.2 确认事实自动提取

**现状**: `fact_extract.rs` 已实现但未被调用

**改动**:
- 在澄清循环中，用户回答后自动调用 `extract_facts(qa_pairs)`
- 提取的确认事实写入 L1 Raw + L2 Index（`store_facts()` 已实现）
- 冲突检测：新事实与已有事实冲突时告警

### 5.3 步骤级上下文压缩

**现状**: `COMPRESSION_PROMPT` 已写，`ContextBuilder` 有价值判断框架

**改动**:
- 在执行循环中，步骤完成后调用压缩
- LLM 逐条判断工具调用价值（高/低）
- 高价值保留原文，低价值压缩为摘要
- 原文存 L1，生成 SourceRef

**验收**: 记忆脑可按权重分级注入推理脑，确认事实自动提取，步骤压缩自动触发

---

## Step 6: 系统提示词对齐

**优先级**: P2

**设计要求** (`design.md:847-875`):
- 感知脑：`sensory.md` — Identity/Rules/Resource Pool/Output Format
- 推理脑执行：`reasoning_execution.md` — Identity/Rules/Safety/Task Adaptation/Memory Context/Output
- 推理脑澄清：`reasoning_clarification.md` — Identity/Analysis Dimensions/Rules/Output Format
- 记忆脑压缩：`memory_compress.md`
- 记忆脑事实提取：`memory_extract.md`
- 主脑评估：`evaluation.md`

**改动**:
- 将设计文档第九章的提示词写入 `~/.ai-brain/prompts/` 目录
- 或内嵌为 `const` 字符串（当前做法）
- 替换现有的简化版 system prompt

**验收**: 各脑使用设计文档定义的完整系统提示词

**状态**: ✅ 已完成 (2026-04-16)

---

## Step 7: 事实提取 LLM 化

**优先级**: P1
**来源**: 架构审计发现 #11

### 7.1 接入 FACT_EXTRACTION_PROMPT

**现状**: `fact_extract.rs` 使用规则引擎（关键词匹配），注释写 "For production would delegate to LLM"。`FACT_EXTRACTION_PROMPT` 已按设计文档 §9.5 对齐（Step 6 完成），但未被调用。

**改动**:
- `fact_extract.rs` 新增 `extract_facts_with_llm()` async 函数
- 持有 `LlmProvider` 引用，调用 `FACT_EXTRACTION_PROMPT`
- 解析 LLM 返回的 `facts[] + conflicts[]`
- `master.rs` 的 `clarify_loop()` 中调用 LLM 版本
- 保留规则版本作为降级路径

**验收**: 澄清循环中使用 LLM 提取确认事实，含冲突检测

**状态**: ✅ 已完成 (2026-04-16)

---

## Step 8: 真实性校验 supports_claim 硬编码修复

**优先级**: P0
**来源**: 架构审计发现 #3

### 8.1 移除硬编码 supports_claim: true

**现状**: `truthfulness.rs:77` 中 `supports_claim` 永远为 `true`，注释写 "实际场景中应由 LLM 判断"

**改动**:
- 引入 `infer_supports_claim()` 函数按来源类型推断支持度
- `Memory(Raw)` → 不假定支持（未经验证）
- 其他来源 → 默认支持
- 代码简洁化：用 `!matches!(layer, MemoryLayer::Raw)` 替代嵌套 match

**验收**: 真实性校验不再硬编码，原始记忆不假定支持声明

**状态**: ✅ 已完成 (2026-04-16)

---

## Step 9: 校验脑降级 — 移除独立副脑

**优先级**: P1
**来源**: 架构审计发现 #7

### 9.1 设计要求

设计文档："校验脑降级为 `guard_check()` 函数，简单前置检查，不需要独立副脑"

### 9.2 改动

**现状**: `ValidationBrain` 仍作为独立副脑参与广播循环，与 `guard_check.rs` 功能重复

**改动**:
- `brain-validation` crate 保留为库（truthfulness/safety 模块仍有价值）
- 移除 `ValidationBrain` 的广播循环参与
- 编排器不再创建 ValidationBrain 实例
- 所有校验通过 `guard_check()` 函数调用
- `evaluate_default_raw()` 移除 validation 快照
- 副脑任务数从 4 降至 2（记忆脑+推理脑）

**验收**: ValidationBrain 不再作为独立副脑运行

**状态**: ✅ 已完成 (2026-04-16)

---

## Step 10: 执行脑合并 — 移除独立副脑

**优先级**: P1
**来源**: 架构审计发现 #6

### 10.1 设计要求

设计文档 1.3 节："执行脑合并到推理脑，推理脑直接调工具，无需中间人"

### 10.2 改动

**现状**: `MotorBrain` 仍作为独立副脑运行，`execute_stub()` 是空壳，真正的工具执行在推理脑的 `tool_loop` 中通过 `UnifiedToolExecutor` 完成

**改动**:
- `brain-motor` crate 保留 `UnifiedToolExecutor` 和 `ToolRegistry`（被推理脑使用）
- 移除 `MotorBrain` 的广播循环参与
- 移除 `execute_stub()` 空壳（仍在 crate 中但不再实例化）
- 编排器不再创建 MotorBrain 实例
- 保留 `UnifiedToolExecutor` 作为 `ToolExecutor` 的实现
- import 清理：移除 `MotorBrain`, `MotorConfig`, `ValidationBrain`, `ValidationConfig`

**验收**: MotorBrain 不再作为独立副脑运行

**状态**: ✅ 已完成 (2026-04-16)

---

## Step 11: tools crate stub 工具修复

**优先级**: P1
**来源**: 架构审计发现 #4, #5

### 11.1 MCP 工具代理连接

**现状**: `run_mcp_tool()` 返回 `result: null, "MCP tool proxy not yet connected"`

**改动**: 返回明确的错误而非假数据

### 11.2 RemoteTrigger / TestingPermission 去除 stub

**现状**: 返回固定 stub 字符串

**改动**:
- `run_remote_trigger()`: 实现真实的 HTTP 请求触发（使用 reqwest）
- `run_testing_permission()`: 返回 `permitted: false` + 说明原因

**验收**: 这 3 个工具不再返回无意义的固定值

**状态**: ✅ 已完成 (2026-04-16)

---

## Step 12: 记忆脑关键词提取升级

**优先级**: P2
**来源**: 架构审计发现 #8

### 12.1 extract_keywords 升级

**现状**: `memory_brain.rs:394` 标注 "简化版，基于规则分词"

**改动**:
- 多分隔符切分 + 句号二次切分
- 中英文停用词过滤（约 60 个常见停用词）
- 去重 + 英文统一小写
- 用 `str::trim` 方法引用替代闭包

**验收**: 关键词提取质量显著提升

**状态**: ✅ 已完成 (2026-04-16)

---

## Step 13: 主脑规则评估降级路径增强

**优先级**: P2
**来源**: 架构审计发现 #9

### 13.1 rule_evaluate_step 增强

**现状**: 降级评估只看 `result.success` 和 `output.is_empty()`

**改动**:
- 检查 `result.output` 实质性内容（> 50 字符）→ +0.1
- 检查 `result.files_modified` 非空 → +0.1
- 检查 `result.tool_history` 非空 → +0.05
- 检查 `step.goal` 关键词在 output 中匹配比例 → +0.15 * hit_ratio
- 基础分 0.5，阈值 0.7
- 返回具体 issues 列表

**验收**: 规则降级评估不再过于宽松

**状态**: ✅ 已完成 (2026-04-16)

## 执行顺序与依赖关系

```
Step 1 (主脑重构)  ←── 一切的前提 ✅
    │
    ├── Step 2 (三阶段模型)  ←── 依赖 Step 1 ✅
    │       │
    │       ├── Step 4 (推理脑桥接)  ←── 依赖 Step 2 的循环
    │       │
    │       └── Step 5 (记忆脑高层接入)  ←── 依赖 Step 2 的循环
    │
    ├── Step 3 (感知脑补全)  ←── 依赖 Step 1（独立，可与 Step 2 并行）
    │
    └── Step 6 (提示词对齐)  ←── 独立 ✅
    │
    ├── Step 7 (事实提取 LLM 化)  ←── 独立
    │
    ├── Step 8 (真实性校验硬编码修复)  ←── 独立
    │
    ├── Step 9 (校验脑降级)  ←── 独立
    │
    ├── Step 10 (执行脑合并)  ←── 独立，可与 Step 9 并行
    │
    ├── Step 11 (tools stub 修复)  ←── 独立
    │
    ├── Step 12 (关键词提取升级)  ←── 独立
    │
    └── Step 13 (规则评估增强)  ←── 独立
```

---

## 预期成果

| 指标 | 修复前 | 修复后 |
|------|--------|--------|
| 主脑完成度 | 15% | 95% |
| 感知脑完成度 | 70% | 95% |
| 推理脑完成度 | 75% | 95% |
| 记忆脑完成度 | 60% | 90% |
| 三阶段模型 | 不存在 | 完整实现 |
| 澄清循环 | 不存在 | 完整实现 |
| 执行循环 | 不存在 | 完整实现 |
| 记忆主动注入 | 不存在 | 完整实现 |