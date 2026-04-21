# AI Brain v2 开发任务

> 基于 v2 架构设计（`docs/architecture/v2/`）的开发任务拆解

## 开发原则

1. **每个任务完成后自检**：
   - 无 TODO/FIXME/HACK 注释
   - 实现与设计文档一致
   - `cargo clippy --workspace --all-targets -- -D warnings` 零错误
   - `cargo test --workspace` 全部通过
2. **按序执行**，后续任务依赖前置任务
3. **每个任务产出可编译可测试的代码**

---

## Phase 1: 清理与基础

### Task 1: 删除旧 crate

**目标**: 删除不再需要的 crate，保留计划文档。

**删除**:
- `rust/crates/brain-sensory/`
- `rust/crates/brain-motor/`
- `rust/crates/brain-validation/`
- `rust/crates/brain-evolution/`
- `rust/crates/brain-bus/`
- `rust/crates/brain-master/`
- `rust/crates/brain-reasoning/`

**保留**:
- `rust/crates/brain-core/` → 清理（Task 2）
- `rust/crates/brain-llm/` → 100% 复用
- `rust/crates/brain-memory/` → 重构（Task 5）
- `rust/crates/brain-integration-tests/` → 重写（Task 10）
- `rust/crates/ai-brain-cli/` → 重构（Task 7-8）
- `rust/crates/tools/` → 复用

**操作**:
1. 从 `rust/Cargo.toml` workspace members 中移除
2. 删除 crate 目录
3. 清理 `ai-brain-cli/Cargo.toml` 中对这些 crate 的依赖
4. 清理 `brain-integration-tests/Cargo.toml` 中的依赖
5. 验证: `cargo check --workspace`

**自检**:
- [ ] workspace 编译通过（现有代码会报错，因为依赖被删了，这是预期的，Task 2-3 修复）
- [ ] 旧的架构设计文档保留在 `docs/architecture/`

---

### Task 2: 重构 brain-core

**目标**: 清理旧类型，新增 v2 所需类型。

**删除/清理**:
- `BrainAgent` trait (agent.rs) — 不再使用
- `BroadcastMessage`, `CollaborationMessage` (types.rs) — 不再使用广播模式
- `BrainResponse`, `BrainResponsePayload` (types.rs) — 旧的响应载荷
- `BrainKind` 枚举调整为: `Main, Memory, Eval`
- `TaskPhase` 旧枚举
- `StatelessBrain` 概念
- `ThinkContext`, `SlowThinkResult` — 旧快慢思考相关

**保留**:
- `BrainId`, `Weight`, `BrainContext`
- `KnowledgeSource`, `TurnUsage`
- `ProgressEvent` — 增强（加 ToolStart.input, ToolDone.output 等）
- `ToolCall`, `ToolExecutionResult`, `ToolDescriptor`
- `ToolExecutor` trait (tool_executor.rs)
- `GuardResult`, guard_check (guard_check.rs)
- `StepResult`, `ToolCallRecord`, `TaskPlan` (plan.rs)
- `AmbiguityReport` (evaluation.rs)
- `BrainConfig`, `ThresholdConfig` (config.rs)
- `CoreError` (error.rs)

**新增**:
```rust
// types.rs
pub struct ConversationMessage {
    pub role: MessageRole,
    pub blocks: Vec<ContentBlock>,
    pub timestamp: Option<DateTime<Utc>>,
}

pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

pub struct UserProfile { ... }
pub struct PitfallRecord { ... }
pub struct EvolutionRule { ... }
pub struct BrainState { ... }  // 记忆脑生成的上下文快照
```

**ProgressEvent 增强**:
```rust
pub enum ProgressEvent {
    Connecting { brain: String, model: String },
    Thinking { brain: String },
    ToolStart { brain: String, tool_name: String, input: String },
    ToolDone { brain: String, tool_name: String, duration_ms: u64,
               output_preview: String, is_error: bool },
    MemoryInjected { count: usize, preview: String },
    EvaluationStart,
    EvaluationResult { passed: bool, issues: Vec<String> },
    LlmRetry { attempt: u32, max_attempts: u32, error: String },
    Done,
}
```

**自检**:
- [ ] `cargo clippy -p brain-core -- -D warnings` 零错误
- [ ] `cargo test -p brain-core` 全部通过
- [ ] 无 TODO 注释

---

### Task 3: 创建 brain-main crate

**目标**: 新建 `brain-main` crate，实现主脑骨架。

**结构**:
```
brain-main/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── main_brain.rs      ← 主脑结构体 + process_input()
    ├── tool_loop.rs       ← LLM↔工具循环（从 brain-reasoning 迁移并增强）
    ├── conversation.rs    ← 对话历史管理
    ├── prompts.rs         ← 主脑系统提示词
    └── error.rs           ← 错误类型
```

**实现**:

1. `conversation.rs` — ConversationMessage 管理:
   - `ConversationHistory` 结构体: `Vec<ConversationMessage>` + token 估算
   - `push_user()`, `push_assistant()`, `push_tool_result()`
   - `estimate_tokens()` — chars/4 估算
   - `is_context_full(threshold)` — 判断是否超过阈值
   - `clear_and_rebuild(brain_state, recent_turns)` — 上下文重建

2. `tool_loop.rs` — 从旧 `brain-reasoning/src/tool_loop.rs` 迁移:
   - `execute_step()` → 改为 `run_tool_loop()`
   - 增加 `ProgressEvent` 中的 input/output_preview 字段
   - 重试间隔从 60s 改为 10s

3. `prompts.rs` — 主脑系统提示词（对齐设计文档）

4. `main_brain.rs` — 主脑核心:
   - `MainBrain::new()`
   - `process_input()` — 处理一轮用户输入
   - `get_messages()` — 供记忆脑/评估脑读取
   - `rebuild_context()` — 供记忆脑触发上下文重建

5. `error.rs` — 错误类型

**依赖**: brain-core, brain-llm

**自检**:
- [ ] `cargo clippy -p brain-main -- -D warnings` 零错误
- [ ] `cargo test -p brain-main` 全部通过
- [ ] tool_loop 单元测试覆盖
- [ ] conversation.rs token 估算测试
- [ ] 无 TODO

---

## Phase 2: 核心实现

### Task 4: 完善主脑 — 完整 tool_loop + 对话历史

**目标**: 主脑能够完整处理用户输入，跑通 tool_loop。

**实现**:
1. `MainBrain::process_input()` 完整流程:
   - 接收用户输入 → 追加到 history
   - 估算 token → 判断是否需要压缩
   - 跑 tool_loop → 获得结果
   - 返回输出

2. tool_loop 增强:
   - 工具参数展示（input 截断到 80 字符）
   - 工具结果展示（output_preview 截断到 100 字符）
   - 错误工具红色标记

3. 对话历史管理:
   - 全量追加模式（对齐 Claude Code）
   - 上下文重建接口（供记忆脑调用）

**测试**:
- 用 `StubToolExecutor` 测试 tool_loop 完整流程
- 测试 token 估算准确性
- 测试上下文重建功能

**自检**:
- [ ] `cargo clippy/test -p brain-main` 通过
- [ ] tool_loop 测试覆盖: 正常流程、工具失败、重试、上下文满
- [ ] 无 TODO

---

### Task 5: 重构记忆脑

**目标**: 重构 `brain-memory` crate 为后台监听模式 + 四步分析。

**结构**:
```
brain-memory/src/
├── lib.rs
├── memory_brain.rs      ← 重写为后台监听模式
├── raw_layer.rs         ← 复用 L1 全量存储
├── index_layer.rs       ← 复用 L2 索引
├── experience_pack.rs   ← 复用 L3 经验包
├── consolidation.rs     ← 重写为四步分析
├── recall.rs            ← 复用三层召回
├── storage.rs           ← 复用文件系统存储
├── user_profile.rs      ← 新建：用户画像
├── pitfall.rs           ← 新建：踩坑库
├── evolution.rs         ← 新建：自进化规则
├── brain_state.rs       ← 新建：上下文快照
├── prompts.rs           ← 重写：四步分析 prompt
└── error.rs
```

**实现**:

1. `memory_brain.rs` — 后台监听:
   - `MemoryBrain::new(llm, storage)`
   - `on_round_complete()` — 每轮完成时调用，计数器 +1
   - `check_and_trigger()` — 检查是否需要触发四步分析
   - `get_brain_state()` — 获取当前 brain_state
   - `get_pitfalls()` / `get_user_profile()` / `get_evolution_rules()` — 供评估脑读取

2. `consolidation.rs` — 四步分析:
   - `run_four_step_analysis()` — 顺序执行四步
   - 每一步独立函数，上一步输出作为下一步输入
   - LLM 调用 + 结果解析

3. `user_profile.rs` — 用户画像:
   - 存储和更新用户偏好
   - 持久化到文件

4. `pitfall.rs` — 踩坑库:
   - 存储踩坑记录
   - 按类别索引
   - 持久化到文件

5. `evolution.rs` — 自进化规则:
   - 存储和更新规则
   - 持久化到文件

6. `brain_state.rs` — 上下文快照:
   - 四步分析后生成
   - 包含映射索引

**自检**:
- [ ] `cargo clippy/test -p brain-memory` 通过
- [ ] 四步分析 prompt 测试
- [ ] 踩坑库 CRUD 测试
- [ ] 用户画像更新测试
- [ ] 无 TODO

---

### Task 6: 创建评估脑

**目标**: 新建 `brain-eval` crate，实现质量审核。

**结构**:
```
brain-eval/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── eval_brain.rs     ← 评估脑核心
    ├── prompts.rs        ← 评估提示词
    ├── checker.rs        ← 检查逻辑
    └── error.rs
```

**实现**:

1. `eval_brain.rs` — 评估脑核心:
   - `EvalBrain::new(llm)`
   - `evaluate(user_input, ai_output, pitfalls, user_profile, rules)` — 执行评估
   - 返回 `EvalResult { passed, issues }`

2. `prompts.rs` — 评估提示词:
   - 五项检查清单（重复踩坑/违反偏好/偷懒/事实错误/忽略指令）
   - JSON 输出格式

3. `checker.rs` — 检查逻辑:
   - 解析评估结果 JSON
   - 构造反馈消息
   - 判断严重程度

**依赖**: brain-core, brain-llm

**自检**:
- [ ] `cargo clippy/test -p brain-eval` 通过
- [ ] 评估 prompt 测试（mock LLM）
- [ ] JSON 解析测试
- [ ] 反馈消息构造测试
- [ ] 无 TODO

---

## Phase 3: 集成

### Task 7: 重写编排器

**目标**: 简化 `orchestrator.rs`，三脑协调。

**实现**:
1. `Orchestrator` 结构体:
   ```rust
   struct Orchestrator {
       main_brain: Arc<Mutex<MainBrain>>,
       memory_brain: Arc<Mutex<MemoryBrain>>,
       eval_brain: Arc<Mutex<EvalBrain>>,
   }
   ```

2. `query()` / `query_streaming()`:
   - 用户输入 → 主脑处理
   - 主脑输出 → 记忆脑 `on_round_complete()`
   - 主脑输出 → 评估脑 `evaluate()`
   - 评估不通过 → 反馈插入主脑 → 主脑重新处理
   - 通过 → 返回用户

3. 记忆脑后台 tokio task:
   - 持续监听轮次计数
   - 条件满足时触发四步分析

**自检**:
- [ ] 编排器单元测试
- [ ] 三脑协调流程测试
- [ ] 无 TODO

---

### Task 8: 重构 REPL

**目标**: REPL 支持对话历史、增强进度展示。

**实现**:
1. REPL 主循环维护 `conversation_history`
2. 每轮查询后追加 (user, assistant) 到 history
3. 传给 `orchestrator.query_streaming()`
4. 进度展示增强（工具参数、记忆注入、评估结果）
5. 内置命令更新

**自检**:
- [ ] 多轮对话测试
- [ ] 进度显示正确
- [ ] 内置命令正常
- [ ] 无 TODO

---

### Task 9: 配置和初始化更新

**目标**: 更新配置系统和初始化流程。

**实现**:
1. config.toml 新增 `[memory]` 和 `[eval]` section
2. `init.rs` 适配新目录结构
3. `main.rs` 适配新 crate 启动流程

**自检**:
- [ ] 配置加载测试
- [ ] 初始化目录结构正确
- [ ] 无 TODO

---

### Task 10: 集成测试

**目标**: 端到端测试三脑协调。

**测试场景**:
1. 基本查询 → 主脑正确处理 → 评估通过
2. 多轮对话 → 上下文累积 → 记忆脑触发总结
3. 上下文满 → 清空重建 → 继续工作
4. 踩坑 → 记忆脑记录 → 评估脑拦截重复踩坑
5. 用户纠正 → 记忆脑记录 → 评估脑下次拦截
6. 工具调用失败 → 记忆脑记录 → 评估脑提醒

**自检**:
- [ ] 所有集成测试通过
- [ ] `cargo clippy --workspace -- -D warnings` 零错误
- [ ] `cargo test --workspace` 全部通过
- [ ] 无 TODO

---

## 任务依赖图

```
Task 1 (删除旧crate)
  │
  ▼
Task 2 (重构brain-core)
  │
  ├──────────────────┐
  ▼                  ▼
Task 3 (brain-main)  Task 5 (brain-memory)  Task 6 (brain-eval)
  │                  │                  │
  ▼                  │                  │
Task 4 (完善主脑)     │                  │
  │                  │                  │
  └──────┬───────────┘                  │
         ▼                              │
    Task 7 (编排器) ◄───────────────────┘
         │
         ▼
    Task 8 (REPL)
         │
         ▼
    Task 9 (配置)
         │
         ▼
    Task 10 (集成测试)
```

Task 3/5/6 可以并行开发（无相互依赖）。
