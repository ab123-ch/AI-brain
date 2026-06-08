# 进化脑 v2 实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 将进化脑从骨架代码重构为拥有主脑全部能力的自主智能体，支持联网研究、学习消化、生成 SKILL.md 技能文件。

**Architecture:** 复用 Orchestrator 架构创建独立 EvoOrchestrator 实例（独立 LLM-B），通过 EvolutionCoordinator 调度六阶段进化循环，VerificationAgent（独立 LLM-C）做质量验证。EvolutionBacklog 收集运行时问题作为进化驱动力。

**Tech Stack:** Rust, Tokio (async), serde_json (持久化), brain-core (BrainAgent trait), brain-llm (LlmProvider), brain-plugin (SkillCatalog), brain-memory (金字塔存储)

---

## 实施进度（2026-06-08 更新）

### 已完成

| Task | 状态 | 文件 | 测试数 |
|------|------|------|--------|
| Task 1: EvolutionBacklog | ✅ | `backlog.rs` | 4 |
| Task 2: EvoTarget | ✅ | `target.rs` | 5 |
| Task 3: EvoLog + CapabilityTree | ✅ | `evo_log.rs` + `capability_tree.rs` | 6 |
| Task 4: EvolutionTrigger | ✅ | `trigger.rs` | 8 (内嵌) |
| Task 5: EvolutionCoordinator | ✅ | `coordinator.rs` | 7 + 23 (内嵌) |
| 审计修复 | ✅ | `backlog.rs`/`evo_log.rs`/`coordinator.rs`/`error.rs` | 69 全通过 |
| Task 6: CycleRunner | ✅ | `cycle_runner.rs` | 24 (内嵌) |
| Task 14: EvoPrompt | ✅ | `evo_prompt.rs` | 8 (内嵌) |
| Task 13: EvoOrchestrator | ✅ | `evo_orchestrator.rs` | 8 (内嵌) |

### 待实施

| Task | Phase | 说明 |
|------|-------|------|
| Task 6 | Phase 3 | CycleRunner 核心循环框架 |
| Task 7-10 | Phase 3 | 六阶段实现 (感知/研究/学习/合成) |
| Task 11-12 | Phase 3 | 注册 + VerificationAgent |
| Task 13-14 | Phase 4 | EvoOrchestrator + System Prompt |
| Task 15-17 | Phase 5 | Orchestrator 集成 + 命令接口 |
| Task 18-19 | Phase 6 | Backlog 收集集成 |
| Task 20 | Phase 7 | 端到端测试 |

### 已完成模块的关键 API 速查

```
backlog.rs:
  EvolutionBacklog::new(base_dir)
  .add_entry(entry)       // 自动去重+持久化
  .query_by_status(status) -> Vec<&BacklogEntry>
  .query_sorted_by_priority() -> Vec<&BacklogEntry>
  .resolve_entry(id, evo_log_id)  // 自动持久化
  .block_entry(id, reason)        // 自动持久化

target.rs:
  EvoTargetQueue::new(base_dir)
  .add_target(target)     // 自动持久化
  .list_targets() -> &[EvoTarget]
  .sorted_targets() -> Vec<&EvoTarget>  // 按 priority 升序
  .update_status(id, status)
  .update_checkpoint(id, idx, met)

evo_log.rs:
  EvoLogStore::new(base_dir)
  .append(entry)          // 自动持久化
  .query_by_date(prefix) -> Vec<&EvoLogEntry>
  .latest() -> Option<&EvoLogEntry>
  .update_entry(log_id, phases, tokens, skills, resolved_backlog, status)  // 自动持久化

capability_tree.rs:
  CapabilityTree::load(base_dir) -> Result<Self>
  .save(base_dir) -> Result<()>
  .update_with_new_skill(skill_name, domain_hint)

trigger.rs:
  EvolutionTrigger::new(config)
  .should_trigger() -> TriggerDecision
  .touch_activity()
  .set_evolving(bool)
  .set_has_targets(bool)

coordinator.rs:
  EvolutionCoordinator::new(base_dir, config) -> Result<Self>
  .pick_next_target() -> Option<EvoTargetCandidate>
  .has_pending_work() -> bool
  .resolve_target(target_id, evo_log_id, skills)
  .block_target(target_id, reason)
  .log_cycle_start(target_id) -> String  // 返回 log_id
  .log_cycle_end(log_id, phases, tokens, skills, resolved_backlog, status)
  .night_session_summary() -> NightSessionResult

cycle_runner.rs:
  CycleRunner::new(llm, config)
  .run(target) -> Result<CycleResult>          // 六阶段循环
  .phase_perceive(target) -> Result<PerceiveResult>
  .phase_research(perceive) -> Result<ResearchResult>
  .phase_learn(research) -> Result<LearnResult>
  .phase_synthesize(learn) -> Result<SynthesizeResult>
  .phase_register(synthesize) -> Result<RegisterResult>
  .phase_verify(register) -> Result<VerificationResult>

evo_prompt.rs:
  EvoPromptContext { current_target, capability_tree_summary, existing_skill_names, ... }
  build_evo_system_prompt(ctx) -> String       // 五层 system prompt

evo_orchestrator.rs:
  SharedResources { mcp_pool_info, skill_names }
  EvoOrchestrator::new(llm, base_dir, config, shared) -> Result<Self>
  .run_evolution(target) -> Result<CycleResult> // 单目标进化
  .run_night_session() -> NightSessionOutput    // 整夜多目标
```

### 待处理项（非阻塞，后续 Task 中处理）

1. **Severity 重复定义**: backlog.rs (4级) vs idle_scanner.rs (3级) → Phase 3 集成时统一
2. **IdleScanner vs EvolutionTrigger**: 功能重叠 → Phase 5 改造时决定去留
3. **reqwest 依赖**: Cargo.toml 声明但未使用 → 清理
4. **旧骨架未连接**: evolver_brain.rs / evolution_engine.rs 仍用旧架构 → Task 15-17 核心工作

### 新会话接续指南

1. 读取设计文档: `docs/plans/2026-06-08-evolution-brain-v2-design.md`
2. 从 Task 6 (CycleRunner) 开始实施
3. CycleRunner 需要依赖 LlmProvider trait — 参见 `brain-llm/src/lib.rs`
4. MCP 工具调用通过 `brain-mcp/src/client_pool.rs` 的 McpClientPool（当前为 stub）
5. 记忆脑接口参见 `brain-memory/src/pyramid_memory_brain.rs`
6. SkillCatalog 参见 `brain-plugin/src/skill_loader.rs`

---

## Phase 1: 数据模型与 Backlog（基础层）

### Task 1: EvolutionBacklog 数据模型

**Files:**
- Create: `rust/crates/brain-evolver/src/backlog.rs`
- Modify: `rust/crates/brain-evolver/src/lib.rs`
- Test: `rust/crates/brain-evolver/tests/backlog_tests.rs`

**Step 1: 写失败测试**

```rust
// tests/backlog_tests.rs
use brain_evolver::backlog::*;
use tempfile::TempDir;

#[test]
fn test_add_and_query_backlog() {
    let dir = TempDir::new().unwrap();
    let mut backlog = EvolutionBacklog::new(dir.path());

    let entry = BacklogEntry {
        id: "blg_001".into(),
        source: BacklogSource::Eval,
        category: BacklogCategory::KnowledgeGap,
        description: "无法回答 Docker 多阶段构建问题".into(),
        severity: Severity::High,
        frequency: 1,
        status: BacklogStatus::Pending,
        created_at: "2026-06-08T00:00:00Z".into(),
        context_snapshot: Some("用户问 Docker...".into()),
        resolved_at: None,
        evolution_log_id: None,
    };

    backlog.add_entry(entry.clone()).unwrap();

    let pending = backlog.query_by_status(BacklogStatus::Pending);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].description, "无法回答 Docker 多阶段构建问题");
}

#[test]
fn test_dedup_increments_frequency() {
    let dir = TempDir::new().unwrap();
    let mut backlog = EvolutionBacklog::new(dir.path());

    let entry1 = BacklogEntry {
        id: "blg_001".into(),
        source: BacklogSource::Eval,
        category: BacklogCategory::KnowledgeGap,
        description: "无法回答 Docker 多阶段构建问题".into(),
        severity: Severity::High,
        frequency: 1,
        status: BacklogStatus::Pending,
        created_at: "2026-06-08T00:00:00Z".into(),
        context_snapshot: None,
        resolved_at: None,
        evolution_log_id: None,
    };

    backlog.add_entry(entry1).unwrap();

    // 相似描述应去重
    let entry2 = BacklogEntry {
        id: "blg_002".into(),
        source: BacklogSource::User,
        category: BacklogCategory::KnowledgeGap,
        description: "Docker 多阶段构建不会回答".into(),
        severity: Severity::Medium,
        frequency: 1,
        status: BacklogStatus::Pending,
        created_at: "2026-06-08T01:00:00Z".into(),
        context_snapshot: None,
        resolved_at: None,
        evolution_log_id: None,
    };

    backlog.add_entry(entry2).unwrap();

    let pending = backlog.query_by_status(BacklogStatus::Pending);
    assert_eq!(pending.len(), 1); // 去重，只有一条
    assert_eq!(pending[0].frequency, 2); // frequency 递增
}
```

**Step 2: 运行测试验证失败**

Run: `cargo test -p brain-evolver backlog_tests`
Expected: FAIL — `backlog` module not found

**Step 3: 实现 EvolutionBacklog**

```rust
// src/backlog.rs
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BacklogSource {
    Eval,
    User,
    SelfDiagnosis,
    Memory,
    UserDefined,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BacklogCategory {
    KnowledgeGap,
    ToolMissing,
    CodeQuality,
    ReasoningWeakness,
    SkillConflict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BacklogStatus {
    Pending,
    InProgress,
    Resolved,
    Superseded,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacklogEntry {
    pub id: String,
    pub source: BacklogSource,
    pub category: BacklogCategory,
    pub description: String,
    pub severity: Severity,
    pub frequency: u32,
    pub status: BacklogStatus,
    pub created_at: String,
    pub context_snapshot: Option<String>,
    pub resolved_at: Option<String>,
    pub evolution_log_id: Option<String>,
}

pub struct EvolutionBacklog {
    store_path: PathBuf,
    entries: Vec<BacklogEntry>,
}

impl EvolutionBacklog {
    pub fn new(base_dir: &Path) -> Self {
        let store_path = base_dir.join("targets.json");
        let entries = Self::load_from_file(&store_path).unwrap_or_default();
        Self { store_path, entries }
    }

    pub fn add_entry(&mut self, entry: BacklogEntry) -> Result<(), String> {
        // 去重: 检查是否有相似描述
        if let Some(existing) = self.find_similar(&entry.description) {
            existing.frequency += 1;
            // 提升严重度
            if entry.severity == Severity::Critical || existing.severity != Severity::Critical {
                existing.severity = entry.severity.clone();
            }
            self.persist()?;
            return Ok(());
        }
        self.entries.push(entry);
        self.persist()
    }

    pub fn query_by_status(&self, status: BacklogStatus) -> Vec<&BacklogEntry> {
        self.entries.iter().filter(|e| e.status == status).collect()
    }

    pub fn query_sorted_by_priority(&self) -> Vec<&BacklogEntry> {
        let mut entries: Vec<&BacklogEntry> = self.entries.iter()
            .filter(|e| e.status == BacklogStatus::Pending || e.status == BacklogStatus::InProgress)
            .collect();
        entries.sort_by(|a, b| {
            // 先按 source 排序 (user_defined 优先)
            // 再按 severity 排序
            // 再按 frequency 降序
            b.frequency.cmp(&a.frequency)
        });
        entries
    }

    pub fn resolve_entry(&mut self, id: &str, evo_log_id: &str) -> Result<(), String> {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.status = BacklogStatus::Resolved;
            entry.evolution_log_id = Some(evo_log_id.to_string());
            entry.resolved_at = Some(chrono::Utc::now().to_rfc3339());
            self.persist()?;
            Ok(())
        } else {
            Err(format!("Entry {} not found", id))
        }
    }

    fn find_similar(&mut self, description: &str) -> Option<&mut BacklogEntry> {
        let desc_lower = description.to_lowercase();
        let desc_keywords: Vec<&str> = desc_lower.split_whitespace().collect();
        self.entries.iter_mut().find(|e| {
            let existing_lower = e.description.to_lowercase();
            let match_count = desc_keywords.iter()
                .filter(|kw| existing_lower.contains(*kw))
                .count();
            match_count as f32 / desc_keywords.len() as f32 > 0.6
        })
    }

    fn load_from_file(path: &Path) -> Result<Vec<BacklogEntry>, String> {
        let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&data).map_err(|e| e.to_string())
    }

    fn persist(&self) -> Result<(), String> {
        let data = serde_json::to_string_pretty(&self.entries).map_err(|e| e.to_string())?;
        std::fs::write(&self.store_path, data).map_err(|e| e.to_string())
    }
}
```

**Step 4: 运行测试验证通过**

Run: `cargo test -p brain-evolver backlog_tests`
Expected: PASS

**Step 5: 更新 lib.rs 导出**

```rust
// src/lib.rs 新增
pub mod backlog;
```

**Step 6: 提交**

```bash
git add rust/crates/brain-evolver/src/backlog.rs \
        rust/crates/brain-evolver/src/lib.rs \
        rust/crates/brain-evolver/tests/backlog_tests.rs
git commit -m "feat(evolver): EvolutionBacklog 数据模型 + 去重 + 持久化"
```

---

### Task 2: EvolutionTarget 用户目标模型

**Files:**
- Create: `rust/crates/brain-evolver/src/target.rs`
- Test: `rust/crates/brain-evolver/tests/target_tests.rs`

**Step 1: 写失败测试**

```rust
// tests/target_tests.rs
use brain_evolver::evo_target::*;
use tempfile::TempDir;

#[test]
fn test_add_and_list_targets() {
    let dir = TempDir::new().unwrap();
    let mut queue = EvoTargetQueue::new(dir.path());

    queue.add_target(EvoTarget {
        id: "tgt_001".into(),
        direction: "Rust async 编程".into(),
        description: "掌握 async/await, tokio, Future".into(),
        priority: 1,
        status: TargetStatus::Pending,
        checkpoints: vec![
            Checkpoint { desc: "理解 async runtime".into(), met: false },
            Checkpoint { desc: "能写 tokio spawn".into(), met: false },
        ],
        created_at: "2026-06-08T00:00:00Z".into(),
        related_skills: vec!["rust-basics".into()],
    }).unwrap();

    let targets = queue.list_targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].direction, "Rust async 编程");
}

#[test]
fn test_priority_ordering() {
    let dir = TempDir::new().unwrap();
    let mut queue = EvoTargetQueue::new(dir.path());

    queue.add_target(EvoTarget {
        id: "tgt_low".into(),
        direction: "低优先级".into(),
        description: "...".into(),
        priority: 5,
        status: TargetStatus::Pending,
        checkpoints: vec![],
        created_at: "2026-06-08T00:00:00Z".into(),
        related_skills: vec![],
    }).unwrap();

    queue.add_target(EvoTarget {
        id: "tgt_high".into(),
        direction: "高优先级".into(),
        description: "...".into(),
        priority: 1,
        status: TargetStatus::Pending,
        checkpoints: vec![],
        created_at: "2026-06-08T00:00:00Z".into(),
        related_skills: vec![],
    }).unwrap();

    let sorted = queue.sorted_targets();
    assert_eq!(sorted[0].id, "tgt_high");
    assert_eq!(sorted[1].id, "tgt_low");
}
```

**Step 2-5: 实现 + 测试 + 提交**（模式同 Task 1）

`target.rs` 包含 `EvoTarget`, `TargetStatus`, `Checkpoint`, `EvoTargetQueue`。

---

### Task 3: EvolutionLog 进化日志 + CapabilityTree 能力树

**Files:**
- Create: `rust/crates/brain-evolver/src/evo_log.rs`
- Create: `rust/crates/brain-evolver/src/capability_tree.rs`
- Test: `rust/crates/brain-evolver/tests/evo_log_tests.rs`
- Test: `rust/crates/brain-evolver/tests/capability_tree_tests.rs`

核心类型:

```rust
// evo_log.rs
pub struct EvoLogEntry {
    pub id: String,
    pub target_id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub phases: Vec<PhaseRecord>,
    pub total_tokens: u64,
    pub skills_created: Vec<String>,
    pub backlog_resolved: Vec<String>,
    pub status: EvoCycleStatus,  // Running / Completed / Blocked / Cancelled
}

pub struct PhaseRecord {
    pub phase: EvoPhase,  // Perceive / Research / Learn / Synthesize / Register / Verify
    pub duration_secs: u64,
    pub summary: String,
    pub tokens_used: u64,
}

// capability_tree.rs
pub struct CapabilityTree {
    pub last_updated: String,
    pub domains: Vec<SkillDomain>,
    pub gaps: Vec<SkillGap>,
}

pub struct SkillDomain {
    pub name: String,
    pub skills: Vec<String>,
    pub coverage: String,
}

pub struct SkillGap {
    pub domain: String,
    pub missing: Vec<String>,
}
```

测试要点:
- 日志追加 + 按日期查询
- 能力树从 SkillCatalog 自动构建（扫描 skill 名称提取 domain）
- 增量更新：新 skill 添加后只更新受影响的 domain

---

## Phase 2: 触发与调度

### Task 4: EvolutionTrigger 触发检测器

**Files:**
- Create: `rust/crates/brain-evolver/src/trigger.rs`
- Test: `rust/crates/brain-evolver/tests/trigger_tests.rs`

```rust
// trigger.rs
pub struct EvolutionTrigger {
    idle_threshold: Duration,       // 默认 1h
    start_hour: u32,                // 默认 0（凌晨12点）
    last_activity: Arc<Mutex<Instant>>,
    is_evolving: Arc<AtomicBool>,
}

impl EvolutionTrigger {
    pub fn should_trigger(&self) -> TriggerDecision {
        // 检查: 时间 + 空闲 + 非进化中
    }

    pub fn touch_activity(&self) { /* 主脑有用户输入时调用 */ }
    pub fn set_evolving(&self, v: bool) { /* 进化开始/结束时设置 */ }
}
```

测试:
- 空闲不足 1h → 不触发
- 白天 10 点 → 不触发（即使空闲）
- 凌晨 1 点 + 空闲 2h → 触发
- 进化中 → 不触发（防重入）

---

### Task 5: EvolutionCoordinator 调度器

**Files:**
- Create: `rust/crates/brain-evolver/src/coordinator.rs`
- Test: `rust/crates/brain-evolver/tests/coordinator_tests.rs`

```rust
// coordinator.rs
pub struct EvolutionCoordinator {
    target_queue: EvoTargetQueue,
    backlog: EvolutionBacklog,
    capability_tree: CapabilityTree,
    log: EvoLogStore,
    config: EvoConfig,
}

impl EvolutionCoordinator {
    /// 选择下一个进化目标（优先级排序）
    pub fn pick_next_target(&self) -> Option<EvoTargetCandidate> {
        // 1. status=in_progress 的目标（续学）
        // 2. priority 最高的 user_defined
        // 3. severity 高 + frequency 高的 backlog
        // 4. capability_tree gaps
    }

    /// 单次进化循环
    pub async fn run_cycle(&mut self, target: EvoTargetCandidate) -> EvoCycleResult {
        // 驱动六阶段循环，记录日志
    }

    /// 整夜进化（多目标循环）
    pub async fn run_night_session(&mut self, token_budget: u64) -> NightSessionResult {
        // 在 token_budget 内循环处理目标
    }
}
```

测试:
- pick_next_target 优先级排序正确
- token 预算耗尽时停止
- 单目标超时时保存进度

---

## Phase 3: 六阶段进化循环

### Task 6: CycleRunner 核心循环

**Files:**
- Create: `rust/crates/brain-evolver/src/cycle_runner.rs`
- Test: `rust/crates/brain-evolver/tests/cycle_runner_tests.rs`

```rust
// cycle_runner.rs
pub enum EvoPhase {
    Perceive,
    Research,
    Learn,
    Synthesize,
    Register,
    Verify,
}

pub struct CycleRunner {
    llm: Arc<dyn LlmProvider>,
    mcp_pool: Arc<McpClientPool>,          // 联网搜索
    skill_catalog: Arc<SkillCatalog>,       // 读取/写入 skills
    memory_brain: Arc<Mutex<PyramidMemoryBrain>>, // 记忆存取
    evo_backlog: Arc<Mutex<EvolutionBacklog>>,
    config: CycleConfig,
}

pub struct CycleConfig {
    pub max_iterations: u32,           // 默认 3
    pub token_budget_per_target: u64,  // 默认 100K
    pub verify_threshold: f64,         // 默认 70.0
}

impl CycleRunner {
    pub async fn run(&self, target: &EvoTargetCandidate) -> CycleResult {
        let mut iteration = 0;
        loop {
            let perceive_result = self.phase_perceive(target).await?;
            let research_result = self.phase_research(&perceive_result).await?;
            let learn_result = self.phase_learn(&research_result).await?;
            let skills = self.phase_synthesize(&learn_result).await?;
            let registered = self.phase_register(&skills).await?;
            let verify_result = self.phase_verify(&registered).await?;

            if verify_result.passed {
                return CycleResult::Success(verify_result);
            }

            iteration += 1;
            if iteration >= self.config.max_iterations {
                return CycleResult::Blocked(verify_result.feedback);
            }
            // 用反馈回到 Learn 阶段
        }
    }
}
```

每个 phase 方法需要独立测试，使用 mock LlmProvider。

---

### Task 7: Phase 1 感知 — 记忆召回 + 能力缺口分析

**Files:**
- Modify: `rust/crates/brain-evolver/src/cycle_runner.rs`

实现 `phase_perceive()`:
1. 调用记忆脑渐进式召回，查询上次学习进度
2. 读取 CapabilityTree 找到缺口
3. 返回 PerceiveResult（缺口描述 + 上次进度 + 相关 backlog 条目）

测试: mock 记忆脑返回预设数据，验证感知结果包含正确缺口

---

### Task 8: Phase 2 研究 — 联网搜索

**Files:**
- Modify: `rust/crates/brain-evolver/src/cycle_runner.rs`

实现 `phase_research()`:
1. 构造搜索 query（基于感知结果的知识缺口）
2. 调用 MCP 工具 `mcp__firecrawl__search` 或 `mcp__web_reader__webReader`
3. 筛选有价值的结果，提取关键内容
4. 全量写入 L1，浓缩为 ResearchSummary（~5K tokens）
5. 返回 ResearchResult

测试: mock MCP 工具返回预设搜索结果，验证 ResearchSummary 生成

---

### Task 9: Phase 3 学习 — 对话式消化 + 四步分析

**Files:**
- Modify: `rust/crates/brain-evolver/src/cycle_runner.rs`

实现 `phase_learn()`:
1. 将 ResearchSummary 喂入 LLM-B 对话
2. LLM 逐个消化知识点，产出理解笔记
3. 调用记忆脑四步分析（Step0 迭代检测 + Step1-3 提取）
4. 上下文压缩：已消化部分写入 L2/L3
5. 返回 LearnResult（已掌握知识点 + 未解决疑问）

---

### Task 10: Phase 4 合成 — SKILL.md 生成

**Files:**
- Modify: `rust/crates/brain-evolver/src/cycle_runner.rs`

实现 `phase_synthesize()`:
1. 将学习成果整理为 SKILL.md 格式
2. YAML frontmatter: name, description, when_to_use, bootstrap
3. Markdown body: 知识正文 + 示例 + 引用来源
4. 生成核心触发词写入 L4
5. 返回 Vec<SkillDraft>（一个目标可能生成多个 skill）

---

### Task 11: Phase 5 注册 — SkillCatalog 写入

**Files:**
- Modify: `rust/crates/brain-evolver/src/cycle_runner.rs`

实现 `phase_register()`:
1. 写入 `~/.ai-brain/skills/<namespace>/SKILL.md`
2. PluginManager 扫描注册
3. 生成 VerificationSpec
4. spawn VerificationAgent

---

### Task 12: Phase 6 验证 — VerificationAgent

**Files:**
- Create: `rust/crates/brain-evolver/src/verification.rs`
- Test: `rust/crates/brain-evolver/tests/verification_tests.rs`

```rust
// verification.rs
pub struct VerificationAgent {
    llm: Arc<dyn LlmProvider>,       // 独立 LLM-C 实例
    skill_catalog: Arc<SkillCatalog>, // 全量 skills（旧+新）
}

pub struct VerificationSpec {
    pub skill_name: String,
    pub skill_description: String,
    pub knowledge_points: Vec<String>,
}

pub struct VerificationResult {
    pub passed: bool,
    pub score: f64,
    pub question_details: Vec<QuestionResult>,
    pub feedback: String,  // 改进建议（如果 fail）
}

impl VerificationAgent {
    pub async fn verify(&self, spec: &VerificationSpec) -> VerificationResult {
        // 1. 基于 knowledge_points 构造 3-5 个测试问题
        // 2. 带全量 skills 上下文回答每个问题
        // 3. 评估: 独立正确性 + 协同一致性 + 补充价值 + 实用性
        // 4. 打分 + 生成反馈
    }
}
```

测试:
- mock LLM 返回正确回答 → passed=true, score>=70
- mock LLM 返回错误回答 → passed=false, 有 feedback
- 验证全量 skills 上下文被传入（检查 prompt 构造）

---

## Phase 4: EvoOrchestrator + 系统Prompt

### Task 13: EvoOrchestrator 构建

**Files:**
- Create: `rust/crates/brain-evolver/src/evo_orchestrator.rs`
- Modify: `rust/crates/brain-evolver/Cargo.toml`（新增 brain-memory, brain-plugin 依赖）

```rust
// evo_orchestrator.rs
pub struct EvoOrchestrator {
    llm: Arc<dyn LlmProvider>,           // 独立 LLM-B
    conversation: ConversationHistory,    // 独立会话历史
    main_brain: EvoMainBrain,             // 复用主脑逻辑
    eval_brain: Option<EvalBrain>,        // 评估脑
    memory_brain: PyramidMemoryBrain,     // 记忆脑（共享存储）
    skill_catalog: Arc<SkillCatalog>,     // 共享只读
    mcp_pool: Arc<McpClientPool>,         // 共享只读
    cycle_runner: CycleRunner,
    system_prompt: String,
}

impl EvoOrchestrator {
    pub fn new(config: &EvoConfig, shared: SharedResources) -> Result<Self, EvolverError> {
        // 创建独立 LLM-B 连接
        // 构建 system prompt（调用 evo_prompt::build）
        // 初始化独立 conversation history
        // 复用共享的 memory storage / skill catalog / mcp pool
    }

    pub async fn run_evolution(&mut self, target: &EvoTargetCandidate) -> CycleResult {
        self.cycle_runner.run(target).await
    }
}
```

---

### Task 14: 进化脑系统 Prompt 生成器

**Files:**
- Create: `rust/crates/brain-evolver/src/evo_prompt.rs`
- Test: `rust/crates/brain-evolver/tests/evo_prompt_tests.rs`

```rust
// evo_prompt.rs
pub struct EvoPromptContext {
    pub current_target: String,
    pub capability_tree_summary: String,
    pub existing_skill_names: Vec<String>,
    pub related_backlog_entries: Vec<String>,
    pub previous_cycle_notes: Option<String>,
    pub tool_list: Vec<String>,
    pub token_budget: u64,
}

pub fn build_evo_system_prompt(ctx: &EvoPromptContext) -> String {
    // Layer 1: 身份定义
    // Layer 2: 进化方法论
    // Layer 3: 知识上下文
    // Layer 4: 质量标准
    // Layer 5: 运行环境
}
```

测试:
- 生成的 prompt 包含目标描述
- 生成的 prompt 包含已有 skills 列表
- 不同上下文生成不同 prompt

---

## Phase 5: Orchestrator 集成 + 命令接口

### Task 15: 主 Orchestrator 集成

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

改动点:
1. 新增字段: `evo_trigger`, `evo_coordinator`, `evo_backlog`
2. 新增 `spawn_evolution()` 方法：创建 EvoOrchestrator + tokio::spawn
3. 新增 `stop_evolution()` 方法
4. 修复双重锁问题（改为单层 Mutex）
5. 用户输入时调用 `evo_trigger.touch_activity()`
6. 启动时 spawn EvolutionTrigger 定时检查 task

---

### Task 16: 命令接口重写

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/command/evolver_cmd.rs`

改动点:
1. `CommandHandler::Sync` → `CommandHandler::Async`
2. 所有 placeholder 替换为实际调用
3. 新增子命令: target list/add/remove, backlog, report, capability, stop

---

### Task 17: 旧 EvolverBrain 改造

**Files:**
- Modify: `rust/crates/brain-evolver/src/evolver_brain.rs`

改动点:
1. `slow_think()` 接入 EvolutionCoordinator
2. `fast_think()` 关键词列表扩展
3. 新增 `on_activate` / `on_deactivate` 生命周期管理
4. 保留 `engine` 字段向后兼容

---

## Phase 6: Backlog 收集集成

### Task 18: EvalBrain → Backlog 集成

**Files:**
- Modify: `rust/crates/brain-eval/src/eval_brain.rs`（或相关评估逻辑）

改动点:
- 评估结果为 Critical/Warning 时，调用 `Orchestrator::add_backlog_entry()`
- 需要通过回调或 shared Arc 传递 backlog 写入能力

---

### Task 19: 主脑自我诊断 → Backlog

**Files:**
- Modify: `rust/crates/brain-main/src/main_brain.rs`（或 prompts.rs）

改动点:
- system prompt 中引导主脑在遇到知识盲区时记录
- 通过 ProgressEvent 或回调传递到 backlog

---

## Phase 7: 端到端测试

### Task 20: 集成测试 — 完整进化循环

**Files:**
- Create: `rust/crates/brain-evolver/tests/e2e_evolution_tests.rs`

测试场景:
1. 添加进化目标 → 触发 → 感知 → 研究 → 学习 → 合成 → 注册 → 验证通过 → 日志记录
2. 验证失败 → 重试 → 第二次通过
3. 重试 3 次失败 → 标记 blocked
4. 跨夜续学：第一次中断 → 第二次从记忆恢复继续
5. 多目标顺序处理

使用 mock LLM + mock MCP 工具，不依赖外部服务。

---

## 依赖关系

```
Task 1 (Backlog) ─┐
Task 2 (Target)  ─┤
Task 3 (Log+Tree)─┼→ Task 4 (Trigger) → Task 5 (Coordinator)
                   │                        │
                   │                        ▼
                   │           Task 6-12 (CycleRunner 六阶段)
                   │                        │
                   │                        ▼
                   └─────────→ Task 13 (EvoOrchestrator) → Task 14 (Prompt)
                                                            │
                                                            ▼
                                              Task 15 (Orchestrator 集成)
                                              Task 16 (命令接口)
                                              Task 17 (旧 EvolverBrain)
                                                            │
                                                            ▼
                                              Task 18-19 (Backlog 收集)
                                                            │
                                                            ▼
                                              Task 20 (端到端测试)
```

## 估算

| Phase | Tasks | 预计步骤数 |
|-------|-------|-----------|
| Phase 1: 数据模型 | Task 1-3 | ~30 步 |
| Phase 2: 触发调度 | Task 4-5 | ~20 步 |
| Phase 3: 六阶段循环 | Task 6-12 | ~50 步 |
| Phase 4: EvoOrchestrator | Task 13-14 | ~20 步 |
| Phase 5: 集成 | Task 15-17 | ~25 步 |
| Phase 6: Backlog 收集 | Task 18-19 | ~15 步 |
| Phase 7: E2E 测试 | Task 20 | ~15 步 |
| **总计** | **20 Tasks** | **~175 步** |
