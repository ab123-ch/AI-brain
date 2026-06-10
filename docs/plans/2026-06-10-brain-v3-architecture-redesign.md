# 智脑 v3 架构重构：脑·人格·会话

> 日期: 2026-06-10
> 状态: 设计阶段
> 作者: 用户 + Claude 对话共识

## 1. 问题诊断

### 1.1 当前问题

| 问题 | 表现 | 根因 |
|------|------|------|
| 脑的边界模糊 | 进化脑、记忆脑、评估脑功能差异大但都叫"脑" | 缺乏"什么是脑"的明确定义 |
| 进化脑名不副实 | 有独立 LLM 配置但从未真正使用独立 LLM | 它是主脑的一种运行模式，不是独立的认知任务 |
| 记忆无统一入口 | 进化脑需要 PyramidMemoryAccess 绕过 brain-memory | 记忆服务没有统一接口 |
| 人格隔离不彻底 | 人格只影响记忆目录，不影响行为能力 | 人格系统太弱 |
| 多会话不支持 | 同时只能服务一个用户对话 | Orchestrator 单会话设计 |
| 工具描述保守 | LLM 自称"在沙箱中没有 git push 能力" | bash 工具描述误导 |

### 1.2 核心洞察

用户提出的两个关键定义：

**脑（Brain）= 需要独立 LLM 实例的认知任务**

脑的存在理由是"需要不同的模型来完成不同类型的认知工作"。
如果没有不同的认知需求，就不需要新的脑。

**人格（Persona）= 上下文 + 记忆 + 行为的隔离单元**

人格决定了"以什么身份思考"，而不是"用什么模型思考"。
同一主脑切人格后，记忆空间、行为风格、可用能力完全不同。

---

## 2. 架构定义

### 2.1 脑的定义（硬件层）

脑是需要**独立 LLM 实例**才能完成的认知任务。判断标准：

> "这个任务是否需要不同类型的模型，或者需要独立的模型实例来避免干扰？"

| 脑 | 独立 LLM 理由 | 模型建议 |
|---|---|---|
| **主脑 MainBrain** | 核心推理循环，必须常驻 | Claude Opus / GPT-4o (文本强模型) |
| **记忆脑 MemoryBrain** | 四步分析需独立 LLM，不能阻塞对话 | 同主脑或 Haiku (成本优化) |
| **评估脑 EvalBrain** | 第三方独立评估，避免自我评价偏差 | 不同厂商模型 (交叉验证) |
| **视觉脑 VisionBrain** | 多模态认知，文本模型无法完成 | GPT-4o / Gemini (多模态模型) |
| ~~进化脑 EvolverBrain~~ | ~~无独立认知需求~~ | **降格为主脑人格** |

**新增脑的原则：扩展能力边界**

```
主脑 (文本) → 想处理图片 → 加视觉脑 (多模态模型)
主脑 (通用) → 想更快分类 → 加快速脑 (Haiku 小模型)
主脑 (英文) → 想中文更强 → 加中文脑 (国产模型)
```

### 2.2 人格的定义（软件层）

人格是同一主脑的**不同运行身份**，通过 PersonaManager 切换。

```rust
struct Persona {
    id: String,              // "self-evolver", "business-dev", "debug-expert"
    name: String,            // "自我迭代者", "业务开发专家"
    system_prompt: String,   // 角色定义 + 行为约束
    memory_space: PathBuf,   // → personas/{id}/pyramid/ (记忆隔离)
    available_tools: Vec<String>,  // 可用工具集
    autonomy_level: AutonomyLevel, // 自主程度
}

enum AutonomyLevel {
    Interactive,    // 每步需用户确认 (默认)
    SemiAuto,       // 常规操作自动，危险操作需确认
    FullyAuto,      // 完全自主 (进化任务用)
}
```

**人格示例：**

| 人格 ID | 名称 | 特点 | 自主等级 |
|---------|------|------|---------|
| `default` | 默认助手 | 通用对话 | Interactive |
| `self-evolver` | 自我迭代者 | 强主观能动性，沙箱内自主工作 | FullyAuto |
| `business-dev` | 业务开发专家 | 熟悉 Ship-Core 架构规范 | SemiAuto |
| `debug-expert` | 故障排查员 | Zipkin 链路追踪 + 紧急响应 | SemiAuto |
| `study-buddy` | 学习伙伴 | 耐心解释，教学风格 | Interactive |

### 2.3 会话（Session）

会话是一次连续交互的上下文容器。多个会话可并发运行。

```rust
struct Session {
    id: String,
    persona_id: String,
    history: ConversationHistory,
    created_at: DateTime<Utc>,
    source: SessionSource,
    result_tx: Option<mpsc::Sender<SessionEvent>>,
}

enum SessionSource {
    User,                    // 用户主动创建
    BackgroundTrigger,       // 后台自动触发 (如空闲进化)
    Scheduled,               // 定时任务
}
```

---

## 3. 架构总览

```
┌──────────────────────────────────────────────────────┐
│                    Orchestrator                       │
│                                                      │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────┐ │
│  │  Session 1   │  │  Session 2   │  │  Session 3   │ │
│  │  persona:    │  │  persona:    │  │  persona:    │ │
│  │  default     │  │  self-evolver│  │  business-dev│ │
│  │  source:User │  │  source:Auto │  │  source:User │ │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘ │
│         │                 │                  │         │
│         ▼                 ▼                  ▼         │
│  ┌──────────────────────────────────────────────────┐ │
│  │              Session Router                      │ │
│  │  根据 session.persona_id 切换上下文后走统一管线    │ │
│  └──────────────────────┬───────────────────────────┘ │
│                         │                             │
│         ┌───────────────┼───────────────┐             │
│         ▼               ▼               ▼             │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐      │
│  │  MainBrain  │  │MemoryBrain │  │ EvalBrain  │      │
│  │  (文本LLM)  │  │ (分析LLM)  │  │ (审核LLM)  │      │
│  └────────────┘  └────────────┘  └────────────┘      │
│                         │                             │
│         ┌───────────────┼───────────────┐             │
│         ▼               ▼               ▼             │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐      │
│  │VisionBrain │  │ FastBrain  │  │ (更多脑...) │      │
│  │(多模态LLM) │  │ (小模型)   │  │            │      │
│  └────────────┘  └────────────┘  └────────────┘      │
│                                                      │
│  ┌──────────────────────────────────────────────────┐ │
│  │             Shared Services                      │ │
│  │  PersonaManager │ ToolPool │ SkillCatalog │ MCP  │ │
│  └──────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────┘
```

---

## 4. 重构范围

### 4.1 Phase 1: 多会话 Orchestrator（核心）

**目标**: Orchestrator 从单会话改为多会话路由。

**改动文件**:
- `ai-brain-cli/src/orchestrator.rs` — 新增 SessionRouter + sessions HashMap
- `ai-brain-cli/src/session.rs` — 新增 Session struct（或复用 web 的 WebSession）
- `brain-main/src/main_brain.rs` — 支持按 persona 构造不同 system prompt

**核心逻辑**:

```rust
impl Orchestrator {
    /// 处理查询 — 从单会话改为多会话路由
    async fn query_session(&self, session_id: &str, input: &str) -> Result<String> {
        let session = self.sessions.get(session_id)?;

        // 1. 切换人格上下文
        let persona = self.persona_manager.get(&session.persona_id);
        let system_prompt = persona.build_system_prompt();

        // 2. 记忆脑召回该人格的记忆
        let memory_ctx = self.memory_brain.recall_for_persona(
            &session.persona_id, input
        );

        // 3. 主脑处理 (使用人格的 system_prompt + 记忆)
        let result = self.main_brain.process(input, system_prompt, memory_ctx).await;

        // 4. 评估脑评估
        let eval = self.eval_brain.evaluate(&result).await;

        // 5. 记忆脑存储
        self.memory_brain.store_turn_for_persona(&session.persona_id, &result);

        Ok(result)
    }
}
```

### 4.2 Phase 2: 进化脑降格为人格

**目标**: 删除 EvolverBrain 作为独立脑，改为 "self-evolver" 人格。

**删除/改造**:
- `brain-evolver/` crate → 保留工具（Sandbox, TDD, Guard），删除 EvolverBrain struct
- `EvolverBrain` → 删除，功能由主脑 + self-evolver 人格承担
- `EvoOrchestrator` → 删除，由 Orchestrator 的后台会话替代
- `CycleRunner` → 保留为 Skill，可被 self-evolver 人格调用
- `EvolutionTrigger` → 保留触发逻辑，触发时改为创建后台会话

**人格配置** (`~/.ai-brain/personas/self-evolver/config.json`):

```json
{
  "id": "self-evolver",
  "name": "自我迭代者",
  "system_prompt": "你是一个具有强主观能动性的自主开发者。你的目标是持续改进代码质量...",
  "autonomy_level": "FullyAuto",
  "available_tools": ["bash", "read_file", "write_file", "edit_file", "glob_search", "grep_search", "Agent"],
  "memory_space": "personas/self-evolver/pyramid/",
  "triggers": {
    "idle_seconds": 7200,
    "min_interval_seconds": 3600
  }
}
```

**触发流程**:

```
EvolutionTrigger 检测空闲 2h
  → orchestrator.create_session("self-evolver", source=BackgroundTrigger)
  → 后台 tokio::spawn: orchestrator.query_session(session_id, evolution_goal)
  → 完成后: session.result_tx.send(Completed { summary, diff })
  → 用户 UI 收到通知
```

### 4.3 Phase 3: Web UI 进化任务列表

**目标**: Web UI 新增进化任务页面，展示后台任务进度和结果。

**新增文件**:
- `web/static/evolution.html` — 进化任务页面 (或集成到主页面)
- `web/ws_handler.rs` — 新增 evolution 相关消息类型

**UI 设计**:

```
┌─ 进化任务中心 ──────────────────────────────────────┐
│                                                      │
│  进行中                                              │
│  ┌────────────────────────────────────────────────┐  │
│  │ 🔄 优化 async runtime 错误处理              45% │  │
│  │ 阶段: 沙箱测试 3/8 · 已用 12 分钟               │  │
│  │ [查看详情] [暂停] [终止]                        │  │
│  └────────────────────────────────────────────────┘  │
│                                                      │
│  已完成                                              │
│  ┌────────────────────────────────────────────────┐  │
│  │ ✅ TUI 粘贴崩溃修复                   6月9日    │  │
│  │ +120 -15 · 测试全过 · [查看diff] [合并]         │  │
│  └────────────────────────────────────────────────┘  │
│  ┌────────────────────────────────────────────────┐  │
│  │ ✅ 记忆召回性能优化                    6月8日    │  │
│  │ +45 -8 · 已自动合并 · [查看diff]                │  │
│  └────────────────────────────────────────────────┘  │
│                                                      │
└──────────────────────────────────────────────────────┘
```

**WebSocket 消息**:

```json
// 进度推送
{ "type": "evolution_progress", "session_id": "evo-bg-001", "phase": "testing", "step": "3/8", "percent": 45 }

// 完成通知
{ "type": "evolution_completed", "session_id": "evo-bg-001", "summary": "修复了 TUI 粘贴崩溃", "diff_stats": "+120 -15", "tests_passed": true }

// 请求列表
{ "type": "list_evolution_tasks" }
```

### 4.4 Phase 4: 多模型脑注册

**目标**: 不同脑可使用不同厂商的模型。

**改动**:
- `brain-llm/src/` — 新增多 Provider 支持
- `ai-brain-cli/src/orchestrator.rs` — 每个脑绑定不同 LlmProvider

**配置示例** (`~/.ai-brain/config.toml`):

```toml
[brains.main]
provider = "openai_compatible"
model = "claude-opus-4-6"
base_url = "https://api.anthropic.com"

[brains.memory]
provider = "openai_compatible"
model = "claude-haiku-4-5"
base_url = "https://api.anthropic.com"

[brains.eval]
provider = "openai_compatible"
model = "gpt-4o"
base_url = "https://api.openai.com"

[brains.vision]
provider = "openai_compatible"
model = "gpt-4o"
base_url = "https://api.openai.com"
```

### 4.5 Phase 5: 工具描述修复

**目标**: 修正 bash 工具描述，让 LLM 知道它有完整执行权限。

**改动**:
- `crates/tools/src/lib.rs` — bash 工具 description

```rust
// Before:
description: "Execute a shell command in the current workspace."

// After:
description: "Execute a shell command with full user permissions. You can run any command the user can, including git, cargo, npm, docker, etc. Use for system commands, builds, tests, git operations, and any shell tasks."
```

---

## 5. 实施顺序

```
Phase 1: 多会话 Orchestrator     ← 核心，其他依赖它
   ↓
Phase 5: 工具描述修复             ← 独立，可提前做，风险低
   ↓
Phase 2: 进化脑降格为人格         ← 依赖 Phase 1
   ↓
Phase 3: Web UI 进化任务列表      ← 依赖 Phase 2
   ↓
Phase 4: 多模型脑注册             ← 独立，可随时做
```

Phase 5 可以独立先行，改动最小、收益最快。

---

## 6. 不修改的内容

| 组件 | 理由 |
|------|------|
| 记忆脑 MemoryBrain | 架构不变，只需支持按 persona_id 隔离（已部分支持） |
| 评估脑 EvalBrain | 架构不变，继续独立评估 |
| 金字塔存储 PyramidStorage | 已按 persona 隔离，无需改动 |
| PersonaManager | 已有人格切换机制，需扩展 autonomy_level 等字段 |
| TUI | 基础功能不变，后续可选加状态栏后台任务提示 |
| EvolverBrain 的工具 (Sandbox/TDD/Guard) | 保留，降格为可被 self-evolver 人格调用的工具 |

---

## 7. 风险和开放问题

| 问题 | 思路 |
|------|------|
| 多会话共享 MainBrain 的 LLM 连接池 | 同一 LlmProvider 实例 Arc 共享，LLM API 本身支持并发 |
| 后台会话的 LLM 调用成本 | self-evolver 人格可配置为使用小模型 (Haiku) |
| 后台会话写文件的安全边界 | autonomy_level=FullyAuto 仍有 Guard 路径限制 |
| 如何处理后台会话与用户会话同时写同一文件 | 沙箱隔离，后台会话在 worktree 中操作 |
| 人格配置放哪里 | `~/.ai-brain/personas/{id}/config.json`，与金字塔目录同级 |
