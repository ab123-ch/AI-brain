# 智脑自我进化模块 — 设计文档

> 日期：2026-05-01
> 状态：已批准

## 1. 背景与目标

AI Brain v2 目前具备主脑（对话）、记忆脑（分析）、评估脑（质量）三大能力，但缺乏**自我改进**能力。本设计新增「进化脑」，让智脑能够：

- **用户触发**：根据用户下达的进化目标，遵循 TDD 流程修改自身代码
- **空闲研究**：空闲 2 小时后自主扫描代码 + 联网研究前沿理论，产出优化建议
- **安全保障**：所有修改在 Git Worktree 沙箱中进行，绝不破坏正式代码

### 核心规则

1. **目标驱动**：明确知道为什么要改代码
2. **测试先行**：先写测试（正常/异常/边界），再改代码
3. **沙箱隔离**：所有操作在 Git Worktree 副本中执行

## 2. 方案选择

选择**方案 A：独立 `brain-evolver` Crate**。

理由：
- 和现有架构一致（副脑模式：主脑/记忆脑/评估脑都是独立 crate）
- 职责单一：只管「代码进化」，不和副脑生命周期管理混淆
- 沙箱天然隔离：独立 crate 在 worktree 中操作，物理上无法触碰正式代码

## 3. 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│                      Orchestrator                            │
│                                                              │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────────┐  │
│  │ MainBrain │  │ Memory   │  │ EvalBrain│  │ Evolver    │  │
│  │ (主脑)    │  │ Brain    │  │ (评估脑) │  │ (进化脑)   │  │
│  └──────────┘  └──────────┘  └──────────┘  └─────┬──────┘  │
│                                                   │          │
│                                    ┌──────────────┘          │
│                                    ▼                         │
│                         ┌─────────────────────┐              │
│                         │  EvolutionEngine     │              │
│                         │  ┌───────────────┐  │              │
│                         │  │ Sandbox       │  │              │
│                         │  │ (Git Worktree)│  │              │
│                         │  └───────────────┘  │              │
│                         │  ┌───────────────┐  │              │
│                         │  │ TddRunner     │  │              │
│                         │  └───────────────┘  │              │
│                         │  ┌───────────────┐  │              │
│                         │  │ IdleScanner   │  │              │
│                         │  └───────────────┘  │              │
│                         │  ┌───────────────┐  │              │
│                         │  │ EvoTools      │  │              │
│                         │  └───────────────┘  │              │
│                         └─────────────────────┘              │
└─────────────────────────────────────────────────────────────┘
```

## 4. 进化流程

### 4.1 路径一：用户触发的进化（TDD 8 步）

```
1. 目标解析    → LLM 分析目标，提取目标文件、预期效果
2. 创建沙箱    → git worktree add .claw/evo-{id}，基于当前 HEAD
3. 编写测试    → 在沙箱中写测试用例（正常/异常/边界）
4. 验证红态    → cargo test → 确认测试失败（TDD 红态）
5. 实现修改    → 在沙箱中修改代码
6. 验证绿态    → cargo test + cargo clippy + cargo fmt
7. 回归验证    → cargo test --workspace 全量测试
8. 生成报告    → 变更摘要 + 测试结果 + 用户确认 merge 或 discard
```

### 4.2 路径二：空闲自主研究

```
1. 空闲检测    → 2 小时无用户任务
2. 自我扫描    → cargo clippy 警告、重复代码、性能热点
3. 联网研究    → 搜索前沿 agent 理论、论文、博客、框架设计
4. 差距分析    → 场景对比："这个场景我能完成吗？缺什么能力？"
5. 生成建议    → 产出进化建议清单，存储到 memory/evolution/suggestions/
```

> 空闲研究只产出建议，不自动修改代码。需用户确认后进入路径一。

## 5. 核心模块

### 5.1 Sandbox（沙箱管理）

```rust
struct Sandbox {
    repo_path: PathBuf,           // 正式仓库路径（只读）
    worktree_path: PathBuf,       // .claw/evo-{id} 副本路径
    branch_name: String,          // evo/{target}-{timestamp}
    evolution_id: String,         // 唯一进化任务 ID
}
```

方法：create / exec / read_file / write_file / diff / merge / discard / cleanup

约束：所有方法只操作 worktree_path，merge 是唯一影响正式代码的入口。

### 5.2 TddRunner（TDD 流程控制器）

```rust
struct TddRunner {
    sandbox: Sandbox,
    llm: Arc<dyn LlmProvider>,
    max_iterations: u32,          // 默认 5
}
```

阶段：Analyzing → WritingTests → RedVerification → Implementing → GreenCheck → Regression → Reporting

### 5.3 IdleScanner（空闲自主研究）

```rust
struct IdleScanner {
    llm: Arc<dyn LlmProvider>,
    idle_threshold: Duration,     // 默认 2 小时
    last_activity: Instant,
    suggestion_store: PathBuf,    // memory/evolution/suggestions/
}
```

方法：is_idle / self_scan / research_frontier / gap_analysis / save_suggestions

### 5.4 EvoTools（进化专用工具集）

| 工具名 | 能力 | 说明 |
|--------|------|------|
| `evo_read_file` | 文件读取 | 读取沙箱中的文件 |
| `evo_write_file` | 文件写入 | 写入沙箱中的文件 |
| `evo_exec` | 命令执行 | 在沙箱中执行 cargo test/clippy/fmt |
| `evo_git_diff` | Git 操作 | 查看沙箱 diff |
| `evo_git_commit` | Git 操作 | 沙箱内 commit |
| `evo_web_search` | 联网 | 搜索前沿研究 |
| `evo_web_fetch` | 联网 | 获取网页内容 |
| `evo_skill_invoke` | Skills | 调用 Claude Code skills |

## 6. 安全守卫

### 6.1 路径守卫
- 所有文件操作限定在 worktree_path 内
- 禁止路径穿越（../）
- 禁止修改 .git/ 目录

### 6.2 命令白名单
- 允许：cargo test/clippy/fmt/check
- 允许：git diff/log/status/commit
- 禁止：git push/fetch/remote
- 禁止：rm -rf, curl | sh 等

### 6.3 资源限制
- 编译超时：5 分钟
- 测试超时：10 分钟
- 单次进化最大迭代：5 次
- 并行进化任务上限：1 个

### 6.4 合并守卫
- merge 前必须全量测试通过
- merge 前必须用户确认
- merge 后自动 git tag 标记（可回溯）

### 6.5 进化记录
- 所有进化操作记录到 .claw/evo-log/
- 包含：目标/diff/测试结果/时间戳
- 支持事后审计

## 7. 与 Orchestrator 的集成

```rust
// orchestrator.rs 新增
impl Orchestrator {
    async fn start_evolution(&self, goal: String) -> Result<EvolutionId>;
    async fn evolution_status(&self, id: &EvolutionId) -> EvolutionStatus;
    async fn approve_evolution(&self, id: &EvolutionId) -> Result<()>;
    async fn reject_evolution(&self, id: &EvolutionId) -> Result<()>;
    async fn check_idle_evolution(&self);
}
```

## 8. Crate 结构

```
rust/crates/brain-evolver/
├── Cargo.toml
└── src/
    ├── lib.rs              # 公共导出
    ├── evolver_brain.rs    # EvolverBrain（实现 BrainAgent trait）
    ├── evolution_engine.rs # 进化引擎主循环
    ├── sandbox.rs          # Git Worktree 沙箱管理
    ├── tdd_runner.rs       # TDD 流程控制器
    ├── idle_scanner.rs     # 空闲扫描 + 自主研究
    ├── tools.rs            # 进化专用工具集
    ├── guard.rs            # 安全守卫（路径/命令/资源）
    └── skill_bridge.rs     # Skills 调用桥接
```

## 9. 依赖关系

```
brain-evolver 依赖：
  - brain-core（BrainAgent trait, types）
  - brain-llm（LlmProvider trait）
  - tokio（异步运行时）
  - serde + serde_json（序列化）
  - git2 或 shell git 命令（Git 操作）
  - reqwest（联网搜索）
```

## 10. 延后项

- 容器化沙箱（Docker 隔离），当前 git worktree 足够
- 多任务并行进化，当前限制 1 个
- 进化效果量化评估（长期跟踪每次进化的实际效果）
- 自动回滚机制（merge 后监控运行时错误，自动 revert）
