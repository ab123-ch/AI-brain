# 进化脑 v2 设计文档

> 日期: 2026-06-08
> 状态: 已确认，待实施

## 1. 设计目标

将进化脑从一个受限的专用 agent 重构为**拥有主脑全部能力的自主智能体**。进化脑在夜间独立运行，基于用户定义的进化方向和运行时发现的问题，联网研究、学习消化、生成 SKILL.md 技能文件，持续扩展智脑的能力边界。

### 核心原则

1. **完全独立**: 进化脑使用独立 LLM 实例，不触碰主脑的会话和任务
2. **全能力继承**: 进化脑拥有主脑+评估脑+记忆脑的全部能力
3. **问题驱动**: 运行时发现的问题和不足是进化的核心驱动力
4. **全自主执行**: 进化循环自主运行，事后报告，不阻塞用户
5. **成果可验证**: 独立验证代理审核生成质量，不自己考自己

## 2. 整体架构

### 2.1 双系统模型

```
┌─────────────────────────────────────────────────────┐
│                   主系统 (白天运行)                   │
│  ┌──────────────┐  ┌──────────┐  ┌───────────────┐  │
│  │  MainBrain   │  │ EvalBrain│  │ MemoryBrain   │  │
│  │  (LLM-A)     │  │ (LLM-A)  │  │ (LLM-A)       │  │
│  └──────────────┘  └──────────┘  └───────────────┘  │
│         ↕ 共享          ↕             ↕ 共享         │
│  ┌─────────────────────────────────────────────────┐│
│  │  Orchestrator (主)                               ││
│  │  ├─ LLM 实例 A（白天专用）                        ││
│  │  ├─ Shared: MCP Pool / SkillCatalog / 记忆存储   ││
│  │  ├─ EvolutionTrigger（触发检测器）                ││
│  │  └─ EvolutionBacklog（洞察收集）                  ││
│  └─────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────┘
                        │ 凌晨12点 + 空闲1h
                        │ 或 /evo 用户触发
                        ▼
┌─────────────────────────────────────────────────────┐
│                 进化系统 (独立运行)                    │
│  ┌─────────────────────────────────────────────────┐│
│  │  EvoOrchestrator (独立 Orchestrator 实例)        ││
│  │  ├─ LLM 实例 B（进化专用，与 A 完全隔离）         ││
│  │  ├─ 独立的 ConversationHistory                   ││
│  │  ├─ 独立 System Prompt (进化专用)                ││
│  │  ├─ 共享只读: MCP Pool / SkillCatalog            ││
│  │  ├─ 共享读写: 记忆存储 (进化结果写入)             ││
│  │  ├─ 共享读写: SkillWriter (SKILL.md 写入)        ││
│  │  │                                              ││
│  │  │  内含完整脑组件:                               ││
│  │  │  ├─ EvoMainBrain (独立主脑, LLM-B)           ││
│  │  │  ├─ EvoEvalBrain (独立评估, LLM-B)           ││
│  │  │  └─ EvoMemoryBrain (共享记忆存储, LLM-B)     ││
│  └─────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────┐│
│  │  EvolutionCoordinator (调度层)                   ││
│  │  ├─ EvoTargetQueue: 进化目标队列                 ││
│  │  ├─ CapabilityTree: 当前能力树快照               ││
│  │  ├─ CycleRunner: 单次进化循环驱动               ││
│  │  └─ ReportGenerator: 事后进化报告                ││
│  └─────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────┐│
│  │  VerificationAgent (验证子代理, LLM-C)           ││
│  │  ├─ 独立 LLM-C 实例（与 A/B 都隔离）             ││
│  │  ├─ 读取全量 SkillCatalog（新+旧 skills）        ││
│  │  ├─ 构造测试问题 + 评估回答质量                   ││
│  │  └─ 按需 spawn，验证完销毁                       ││
│  └─────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────┘
```

### 2.2 LLM 隔离模型

| 实例 | 用途 | 生命周期 | 配置来源 |
|------|------|---------|---------|
| LLM-A | 主脑白天运行 | 常驻 | 主配置 |
| LLM-B | 进化脑夜间运行 | 进化启动时创建，完成时销毁 | `evolution/llm-config.json` 或 fallback 主配置 |
| LLM-C | 验证子代理 | 每个 skill 验证时 spawn，验证完销毁 | 同 LLM-B 配置但独立连接 |

**硬性约束**: 进化脑的 LLM 绝不操作主脑的任务，三者上下文完全隔离。

### 2.3 共享资源边界

| 资源 | 进化脑访问 | 说明 |
|------|-----------|------|
| MCP Pool | 只读共享 | 调用联网搜索工具不冲突 |
| SkillCatalog | 读+写共享 | 生成新 skill 后主脑立即可用 |
| 记忆存储 | 读+写共享 | 进化结果写入，主脑后续可召回 |
| EvolutionBacklog | 读+写共享 | 主脑白天写入问题，进化脑夜间消费 |
| LLM 连接 | **完全隔离** | 各用各的实例 |

## 3. 进化洞察收集系统（EvolutionBacklog）

进化不只是执行用户定义的目标，运行中发现的问题和不足是更重要的驱动力。

### 3.1 洞察来源

| 来源 | 触发条件 | 写入方式 |
|------|---------|---------|
| EvalBrain | 评估结果为 Critical/Warning | 自动写入，category 按评估类型分类 |
| 用户反馈 | 用户说"你错了"/"你不会"/纠正 | 主脑识别意图后写入 |
| 主脑自我感知 | tool_loop 中遇到知识盲区 | system prompt 引导自我诊断并记录 |
| 记忆脑 | pitfall 记录累积超过阈值 | GuardianEngine 周期扫描时写入 |
| 用户手动 | `/evo target add <描述>` | 命令直接写入 |

### 3.2 去重机制

同类问题（description 语义相近）不新增记录，而是 `frequency++` 并更新 `severity`，高频问题自动提升优先级。

### 3.3 优先级排序

```
1. 用户定义的进化方向 (user_defined)
2. status=in_progress 的目标（续学）
3. 高频 + 高严重度的 backlog 问题
4. 用户定义方向达标后 → 自检代码质量
5. 代码无问题 → 扫描 capability_tree gaps
6. 低频积压问题
```

## 4. 六阶段进化循环（Evolution Cycle）

```
Phase 1: 感知 (Perceive)
├─ 读取目标描述 + backlog 上下文
├─ 记忆脑渐进式召回 L4→L3→L2→L1（跨夜续学）
├─ 扫描 CapabilityTree（已有 skill / 知识覆盖）
└─ 评估缺口：还差什么？需要什么新知识/能力？

Phase 2: 研究 (Research)
├─ 通过 MCP 工具联网搜索（firecrawl / web fetch）
├─ 筛选、提取、整理有价值的信息
├─ 研究资料全量写入 L1（不丢）
└─ 形成 ResearchSummary（浓缩后 ~5K tokens）

Phase 3: 学习 (Learn)
├─ 将研究资料喂入 EvoMainBrain
├─ 对话式学习消化知识
├─ EvoEvalBrain 评估理解程度
├─ 四步分析提取: 事实→模式→经验→触发词
├─ 迭代直到理解度达标
└─ 上下文压缩: 已消化部分浓缩到 L2/L3

Phase 4: 合成 (Synthesize)
├─ 整理为结构化 SKILL.md（frontmatter + body）
├─ EvoEvalBrain 审核 skill 质量
├─ 核心触发词写入 L4（主脑自动召回用）
└─ 不达标则回到 Phase 3 补充学习

Phase 5: 注册 (Register)
├─ 写入 ~/.ai-brain/skills/<namespace>/SKILL.md
├─ PluginManager 自动注册
├─ 生成 VerificationSpec 给验证代理
└─ spawn VerificationAgent（独立 LLM-C）

Phase 6: 验证 (Verify)
├─ 验证代理读取全量 SkillCatalog（新+旧 skills）
├─ 基于 VerificationSpec 构造 3-5 个测试问题
├─ 测试维度: 独立正确性 / 协同一致性 / 补充价值 / 实用性
├─ 输出 VerificationResult (pass/fail + score + 改进建议)
├─ pass (score >= 70) → 完成
├─ fail + 重试 < 3 → 带反馈回到 Phase 3
└─ fail + 重试 >= 3 → 标记 blocked，写入报告
```

### 4.1 验证代理（VerificationAgent）

关键设计: 验证代理拥有全量 skills，不只是新 skill。

```
输入:
├─ SkillCatalog 全量（所有已注册的 skills，旧 + 新）
├─ VerificationSpec（新 skill 的知识点清单）
└─ CapabilityTree（能力树快照，知道整体能力边界）

验证维度:
├─ 独立正确性: 新 skill 知识本身对不对
├─ 协同一致性: 新 skill 和旧 skill 有没有矛盾
├─ 补充价值: 新 skill 是否填补了真正的空白
└─ 实用性: 能否与已有 skills 配合解决复合问题

生命周期: spawn → 执行验证 → 返回结果 → 销毁
```

## 5. 进化脑系统 Prompt

五层结构:

```
Layer 1: 身份定义
├─ 你是智脑的进化子系统，在夜间独立运行
├─ 你拥有主脑的全部能力：推理、工具调用、记忆、评估
├─ 你的使命：持续提升智脑的能力边界
└─ 你当前正在处理的目标：{current_target}

Layer 2: 进化方法论
├─ 六阶段循环：感知→研究→学习→合成→注册→验证
├─ 每个阶段有明确的完成标准
├─ 迭代上限 3 次，超过则报告阻塞
└─ 研究优先用 MCP 工具联网获取一手资料

Layer 3: 知识上下文
├─ 当前能力树：{capability_tree_summary}
├─ 已有 skills 清单：{existing_skill_names}
├─ 本目标的 backlog 记录：{related_backlog_entries}
└─ 上次进化的积累：{previous_cycle_notes}

Layer 4: 质量标准
├─ SKILL.md 必须包含：触发条件 + 知识正文 + 示例
├─ 知识必须来自可验证的来源（标注引用）
├─ 不允许模糊的概括，每个结论必须有具体依据
└─ 与已有 skill 矛盾的知识必须明确标注差异并说明

Layer 5: 运行环境
├─ 当前时间：{now}
├─ 可用工具：{tool_list}
└─ 资源预算：单目标最多消耗 {token_budget} tokens
```

### 与主脑 system prompt 的差异

| 维度 | 主脑 | 进化脑 |
|------|------|--------|
| 身份 | 用户的服务助手 | 自主学习者 |
| 目标来源 | 用户实时输入 | 目标队列 + backlog |
| 时间压力 | 实时响应 | 充足时间深度研究 |
| 输出 | 回答用户问题 | SKILL.md 文件 |
| 验证 | 评估脑实时审核 | 独立验证代理事后审核 |
| 研究深度 | 快速准确即可 | 要求溯源到一手资料 |

## 6. 记忆脑集成

### 6.1 进化循环中记忆脑的参与

```
Phase 1 感知:
├─ 渐进式召回 L4→L3→L2→L1（跨夜续学恢复进度）
└─ 召回主脑运行时的相关 pitfall/经验（backlog 上下文补充）

Phase 2 研究:
├─ 研究资料全量写入 L1（不丢，可回溯）
└─ 每个资料源关键点写入 L2（任务摘要）

Phase 3 学习:
├─ 四步分析: 事实提取→模式识别→经验抽象→触发词
├─ Step0 迭代检测（新知识与已有记忆冲突/补充判断）
├─ 经验抽象写入 L3
├─ 上下文压缩: 已消化部分浓缩到 L2/L3
└─ 保留 token 给未消化的新知识

Phase 4 合成:
└─ 核心触发词写入 L4（主脑遇到相关问题自动召回 skill）

Phase 6 验证:
└─ 验证评判结果+改进建议写入 L1+L2（下次续学时召回）
```

### 6.2 上下文压缩策略

**问题**: Phase 2 研究可能获取 20-50K tokens 的资料，Phase 3 多轮迭代持续增长。

**解决方案: 两条压缩触发线**

```
触发线 1: 阶段边界（主动压缩）

Phase 2 → Phase 3 过渡:
├─ 研究资料全量写入 L1（不丢）
├─ 浓缩引擎把资料压缩为 ResearchSummary（~50K → ~5K tokens）
├─ ResearchSummary 写入 L2
└─ 对话历史用 ResearchSummary 替代原始资料

Phase 3 每轮迭代之间:
├─ 上一轮学习笔记写入 L2
├─ 只保留当前迭代的活跃上下文
└─ 已达标的知识点从对话历史中移除

触发线 2: token 阈值兜底（被动压缩）

当 conversation tokens > compress_at 阈值:
├─ 最近 5 轮对话保持完整
├─ 更早的对话用记忆脑摘要替代
└─ 确保 LLM 始终有足够 context window 工作空间
```

**四项关键保证**:

1. **不丢信息**: 原始资料 → L1 全量保存，压缩只影响对话上下文
2. **压缩后够用**: Phase 3 上下文控制在 ~13K tokens（远低于 context window）
3. **跨夜不依赖对话历史**: 完全靠金字塔召回恢复上下文
4. **浓缩引擎复用现有实现**: 零额外开发成本

### 6.3 跨夜续学机制

```
第一天凌晨: 学 Rust async，Phase 1-3 完成，Phase 4 质量不够
  → 记忆脑保存: 学习进度(L2)、已消化知识(L3)、触发词(L4)
  → token 预算耗尽，标记 status: "in_progress"

第二天凌晨: 续学
  → L4 触发词命中 → L3 经验层 → L2 任务摘要 → L1 全量
  → 拿到: "已掌握 runtime 和错误传播，缺 pinning 和 Send 约束"
  → 直接从 Phase 3 继续，不从头来

第三天凌晨: 合成通过，验证打分 85
  → 生成 SKILL.md + L4 触发词
  → 主脑第二天白天遇到 async 问题 → L4 自动触发 → 召回 skill
```

## 7. 数据模型与持久化

存储位置: `~/.ai-brain/evolution/`

### 7.1 targets.json

```json
[
  {
    "id": "tgt_001",
    "source": "user_defined",
    "direction": "Rust async 编程体系",
    "description": "掌握 Rust async/await、tokio runtime、Future trait 等核心概念",
    "status": "in_progress",
    "priority": 1,
    "created_at": "2026-06-01T10:00:00Z",
    "related_skills": ["rust-basics"],
    "checkpoints": [
      { "desc": "能解释 async runtime 模型", "met": true },
      { "desc": "能处理 async 错误传播", "met": true },
      { "desc": "能编写 tokio 多任务协作", "met": false }
    ]
  },
  {
    "id": "blg_042",
    "source": "eval",
    "category": "knowledge_gap",
    "description": "多次无法正确回答 Docker 多阶段构建优化问题",
    "severity": "high",
    "frequency": 5,
    "status": "pending",
    "created_at": "2026-06-05T14:30:00Z",
    "context_snapshot": "用户问 Docker multi-stage build..."
  }
]
```

### 7.2 evolution_log.json

```json
[
  {
    "id": "evo_20260608_001",
    "target_id": "tgt_001",
    "started_at": "2026-06-08T00:15:00Z",
    "finished_at": "2026-06-08T01:42:00Z",
    "phases": [
      {
        "phase": "perceive",
        "duration_secs": 120,
        "summary": "识别缺口：async runtime 模型和错误处理",
        "tokens_used": 3500
      },
      {
        "phase": "research",
        "duration_secs": 480,
        "sources_fetched": ["tokio.rs docs", "Rust async book ch3-5"],
        "summary": "收集 12 篇资料，提取 34 个知识点",
        "tokens_used": 12000
      }
    ],
    "total_tokens": 48000,
    "skills_created": ["rust-async-basic", "rust-tokio-patterns"],
    "backlog_resolved": ["blg_015", "blg_023"]
  }
]
```

### 7.3 capability_tree.json

```json
{
  "last_updated": "2026-06-08T01:42:00Z",
  "domains": [
    {
      "name": "Rust 编程",
      "skills": ["rust-basics", "rust-error-handling", "rust-async-basic"],
      "coverage": "基础✓ 错误处理✓ async✓ 并发模式✗ 宏✗ FFI✗"
    }
  ],
  "gaps": [
    { "domain": "Rust 编程", "missing": ["宏编程", "FFI", "unsafe"] }
  ]
}
```

## 8. 命令接口

```
/evo                              查看进化系统状态
/evo start <方向描述>              用户即时触发一次进化
/evo target list                  查看进化目标列表
/evo target add <描述>            添加用户定义的进化方向
/evo target remove <id>           移除进化目标
/evo target priority <id> <n>     调整优先级
/evo backlog                      查看 backlog 问题积压
/evo report [last|<id>]           查看进化报告
/evo capability                   查看当前能力树全景
/evo stop                         中止正在运行的进化循环
```

## 9. 触发机制

### 9.1 EvolutionTrigger

运行在主 Orchestrator 内，轻量级，不占 LLM 资源。

**检测条件（全部满足才触发）**:
- 时间 >= 00:00（本地时间）
- 主脑空闲 >= 1h（无用户输入）
- 无正在运行的进化任务（防重入）
- targets.json 中存在 status != resolved 的条目

**触发后**:
1. 主 Orchestrator 调用 `spawn_evolution()`
2. 创建 EvoOrchestrator（独立 LLM-B）
3. 在后台 tokio::task 中运行
4. 主 Orchestrator 进入"进化中"状态
5. 用户此时发消息 → 通知"进化脑正在运行，是否中断？"
6. 进化完成后自动销毁 EvoOrchestrator，释放资源

## 10. 资源安全

### 10.1 Token 预算

- 单目标上限: 100K tokens（可配置）
- 单夜总预算: 500K tokens（防止失控）
- 预算耗尽 → 保存进度，第二天续学

### 10.2 时间保护

- 单目标最长 1h（可配置）
- 总运行最长 4h（到凌晨 4 点强制停止）
- 超时 → 保存进度，生成报告

### 10.3 并发保护

- 进化脑运行时主脑拒绝新进化触发
- 用户消息中断: "进化脑正在运行，是否停止？"
- 用户确认停止 → `stop_evolution()` → 保存进度

## 11. 白天主脑的进化感知

**主脑启动时（每天首次启动）**:
- 读取 evolution_log.json 最后一夜记录
- 读取 capability_tree.json diff（昨晚新增了什么 skill）
- 通知用户: "昨晚进化脑完成了 X 个目标，新增 Y 个 skill"

**主脑运行中**:
- 新 skill 自动出现在 SkillCatalog 中（无需手动加载）
- L4 潜意识触发词自动生效（遇到相关问题会召回新 skill）
- 实战中发现新 skill 有问题 → 写入 backlog

## 12. Orchestrator 集成改动

### 新增字段

```rust
// orchestrator.rs
evo_trigger: Arc<EvolutionTrigger>
evo_coordinator: Arc<Mutex<Option<EvolutionCoordinator>>>
evo_backlog: Arc<Mutex<EvolutionBacklog>>
```

### 新增方法

```rust
spawn_evolution(target: Option<String>)    // 启动进化
stop_evolution()                           // 中止进化
evo_status() -> EvoStatus                  // 查询状态
evo_report(id: Option<String>) -> Report   // 查看报告
add_backlog_entry(entry: BacklogEntry)     // 写入 backlog
```

### EvoOrchestrator 创建

- 读取独立配置: `~/.ai-brain/evolution/llm-config.json`
- 没有配置 → fallback 到主 LLM 配置但使用独立连接实例
- 复用 Orchestrator 的构建逻辑
- 注入进化专用 system prompt
- tokio::spawn 后台运行

## 13. 现有代码处置

### 保留（已完成且可复用）

- `brain-evolver/src/sandbox.rs` — Git Worktree 沙箱（完整实现）
- `brain-evolver/src/guard.rs` — 安全守卫（完整实现）
- `brain-evolver/src/error.rs` — 错误类型（可复用）
- Orchestrator 中 `evolver` 字段和 5 个暴露方法（保留兼容）

### 重写

- `brain-evolver/src/evolver_brain.rs` — 从骨架重写为 EvoOrchestrator 模式
- `brain-evolver/src/evolution_engine.rs` — 从状态机重写为六阶段循环
- `brain-evolver/src/tdd_runner.rs` — 重写为 CycleRunner
- `brain-evolver/src/idle_scanner.rs` — 重写为 EvolutionTrigger + EvolutionCoordinator

### 新增

- `brain-evolver/src/coordinator.rs` — EvolutionCoordinator 调度层
- `brain-evolver/src/cycle_runner.rs` — 六阶段循环驱动
- `brain-evolver/src/backlog.rs` — EvolutionBacklog 洞察收集
- `brain-evolver/src/capability_tree.rs` — 能力树快照管理
- `brain-evolver/src/verification.rs` — VerificationAgent 独立验证
- `brain-evolver/src/evo_prompt.rs` — 进化脑系统 prompt 生成
- `brain-evolver/src/report.rs` — 进化报告生成
- `ai-brain-cli/src/command/evolver_cmd.rs` — 重写命令 handler（替换 placeholder）
